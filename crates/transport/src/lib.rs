//! `reqwest` adapter for the restbook engine.
//!
//! The important detail here is runtime isolation. `reqwest` needs a tokio
//! reactor, while GUI frameworks (GPUI included) run their own executor. So this
//! type owns a tokio runtime on a dedicated thread and exposes only plain
//! futures: callers `.await` from whatever executor they happen to be on.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::channel::oneshot;
use reqwest::redirect::Policy;
use reqwest::Client;
use restbook_core::{
    CancelToken, Error, PartData, PreparedBody, PreparedRequest, ResponseRecord, Result, Transport,
};

const MAX_REDIRECTS: usize = 10;
const CANCEL_POLL: Duration = Duration::from_millis(25);

pub struct ReqwestTransport {
    runtime: tokio::runtime::Handle,
    clients: Clients,
}

impl ReqwestTransport {
    pub fn new() -> std::io::Result<Self> {
        let (sender, receiver) = std::sync::mpsc::channel();

        std::thread::Builder::new()
            .name("restbook-http".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        return;
                    }
                };
                let _ = sender.send(Ok(runtime.handle().clone()));
                // Park this thread for the life of the process: it owns the
                // runtime, and spawned tasks keep running on it.
                runtime.block_on(std::future::pending::<()>());
            })?;

        let runtime = receiver
            .recv()
            .map_err(|_| std::io::Error::other("http runtime thread exited during start"))??;

        Ok(Self {
            runtime,
            clients: Clients::build()?,
        })
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new().expect("failed to start the restbook http runtime")
    }
}

/// Redirect policy lives on the client, not the request, so each combination is
/// built once and reused (connection pools are per client).
struct Clients {
    follow: Client,
    follow_insecure: Client,
    manual: Client,
    manual_insecure: Client,
}

impl Clients {
    fn build() -> std::io::Result<Self> {
        Ok(Self {
            follow: build_client(true, false)?,
            follow_insecure: build_client(true, true)?,
            manual: build_client(false, false)?,
            manual_insecure: build_client(false, true)?,
        })
    }

    fn pick(&self, follow_redirects: bool, insecure: bool) -> Client {
        match (follow_redirects, insecure) {
            (true, false) => self.follow.clone(),
            (true, true) => self.follow_insecure.clone(),
            (false, false) => self.manual.clone(),
            (false, true) => self.manual_insecure.clone(),
        }
    }
}

fn build_client(follow_redirects: bool, insecure: bool) -> std::io::Result<Client> {
    let mut builder = Client::builder()
        .user_agent(concat!("restbook/", env!("CARGO_PKG_VERSION")))
        // One jar per client: cookies survive across requests, like a browser.
        .cookie_store(true)
        .redirect(if follow_redirects {
            Policy::limited(MAX_REDIRECTS)
        } else {
            Policy::none()
        });

    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }

    builder.build().map_err(std::io::Error::other)
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn execute(
        &self,
        request: PreparedRequest,
        cancel: CancelToken,
    ) -> Result<ResponseRecord> {
        let client = self
            .clients
            .pick(request.follow_redirects, request.insecure);

        // Bridge: hand the work to the tokio runtime, await a plain channel.
        let (sender, receiver) = oneshot::channel();
        self.runtime.spawn(async move {
            let _ = sender.send(run(client, request, cancel).await);
        });

        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(Error::Transport("request task was dropped".to_owned())),
        }
    }
}

async fn run(
    client: Client,
    request: PreparedRequest,
    cancel: CancelToken,
) -> Result<ResponseRecord> {
    let method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
        .map_err(|error| Error::Transport(error.to_string()))?;

    let mut builder = client.request(method, request.url.clone());
    if let Some(timeout_ms) = request.timeout_ms {
        builder = builder.timeout(Duration::from_millis(timeout_ms));
    }
    for (key, value) in &request.headers {
        builder = builder.header(key.as_str(), value.as_str());
    }

    builder = apply_body(builder, request.body).await?;

    let started = Instant::now();
    let send = builder.send();
    tokio::pin!(send);

    let response = tokio::select! {
        result = &mut send => result.map_err(|error| Error::Transport(error.to_string()))?,
        _ = wait_for_cancel(cancel.clone()) => return Err(Error::Cancelled),
    };

    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or_default().to_owned(),
            )
        })
        .collect::<Vec<_>>();

    let body = tokio::select! {
        result = response.bytes() => result.map_err(|error| Error::Transport(error.to_string()))?,
        _ = wait_for_cancel(cancel) => return Err(Error::Cancelled),
    };

    Ok(ResponseRecord {
        status,
        headers,
        body: body.to_vec(),
        duration_ms: started.elapsed().as_millis() as u64,
        received_at: chrono::Utc::now(),
    })
}

async fn apply_body(
    builder: reqwest::RequestBuilder,
    body: PreparedBody,
) -> Result<reqwest::RequestBuilder> {
    Ok(match body {
        PreparedBody::None => builder,
        PreparedBody::Bytes { mime, data } => {
            let builder = if mime.is_empty() {
                builder
            } else {
                builder.header(reqwest::header::CONTENT_TYPE, mime)
            };
            builder.body(data)
        }
        PreparedBody::Form(fields) => builder.form(&fields),
        PreparedBody::File(path) => {
            let data = read_file(&path).await?;
            builder.body(data)
        }
        PreparedBody::Multipart(parts) => {
            let mut form = reqwest::multipart::Form::new();
            for part in parts {
                form = match part.data {
                    PartData::Text(text) => form.text(part.name, text),
                    PartData::File {
                        path,
                        mime,
                        file_name,
                    } => {
                        let data = read_file(&path).await?;
                        let mut file = reqwest::multipart::Part::bytes(data).file_name(file_name);
                        if let Some(mime) = mime {
                            file = file
                                .mime_str(&mime)
                                .map_err(|error| Error::Transport(error.to_string()))?;
                        }
                        form.part(part.name, file)
                    }
                };
            }
            builder.multipart(form)
        }
    })
}

async fn read_file(path: &str) -> Result<Vec<u8>> {
    tokio::fs::read(path)
        .await
        .map_err(|error| Error::Transport(format!("{path}: {error}")))
}

async fn wait_for_cancel(cancel: CancelToken) {
    loop {
        if cancel.is_cancelled() {
            return;
        }
        tokio::time::sleep(CANCEL_POLL).await;
    }
}
