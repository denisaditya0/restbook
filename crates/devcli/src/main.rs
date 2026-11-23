//! Development harness for the restbook engine.
//!
//! This is not a product feature. It exists so the request pipeline can be
//! exercised, debugged, and regression-checked without a window.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use restbook_core::import::{self, ImportFormat, Options};
use restbook_core::{
    prepare, Body, CancelToken, Error, HistoryEntry, Kv, Method, OAuthConfig, OAuthGrant,
    OAuthTransport, Origin, Progress, ProgressSnapshot, RequestDef, Result, Store, Transport,
    VarContext,
};
use restbook_store::{FolderStore, JsonlStore};
use restbook_transport::ReqwestTransport;

#[derive(Parser)]
#[command(name = "devcli", about = "Drive the restbook engine from a terminal")]
struct Cli {
    /// Where the dev history is written (plain JSONL, not the app's store).
    #[arg(long, global = true, default_value = ".restbook/history.jsonl")]
    history: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prepare and send one request, then record it in history.
    Send {
        url: String,

        #[arg(short = 'X', long, default_value = "GET")]
        method: String,

        /// Repeatable: `-H "Accept: application/json"`.
        #[arg(short = 'H', long = "header", value_name = "KEY:VALUE")]
        headers: Vec<String>,

        /// Raw JSON body.
        #[arg(long)]
        json: Option<String>,

        /// Skip TLS certificate verification.
        #[arg(long)]
        insecure: bool,

        /// Report sent/received byte counts and elapsed time while the request
        /// runs, exercising `Transport::execute_with_progress`.
        #[arg(long)]
        progress: bool,

        /// Repeatable: `--var TOKEN=abc`.
        #[arg(long = "var", value_name = "KEY=VALUE")]
        vars: Vec<String>,
    },

    /// Search recorded history. An empty query lists the most recent entries.
    History {
        #[arg(default_value = "")]
        query: String,

        #[arg(short = 'n', long, default_value_t = 20)]
        limit: u32,
    },

    /// Parse a file as a curl command, Postman, HAR, or OpenAPI document.
    Import {
        /// The file to read. `-` reads standard input.
        file: PathBuf,

        /// Override format detection: curl, postman, har, openapi, or bundle.
        #[arg(long)]
        format: Option<String>,

        /// Show the parsed requests instead of only the counts.
        #[arg(long)]
        verbose: bool,
    },

    /// Read a supported file and write it into a folder workspace on disk.
    ///
    /// This is the Git-friendly shape: one file per collection, with the
    /// workspace marker, a `.gitignore`, and history kept out of the tree.
    Export {
        /// The file to read. `-` reads standard input.
        file: PathBuf,

        /// The workspace directory. Created if it does not exist.
        #[arg(long, value_name = "DIR")]
        workspace: PathBuf,

        /// Override format detection: curl, postman, har, openapi, or bundle.
        #[arg(long)]
        format: Option<String>,
    },

    /// Fetch an OAuth 2.0 token, exercising `OAuthTransport`.
    ///
    /// With the authorization-code grant and no `--code`, this prints the URL
    /// to open and the PKCE verifier/state to keep; rerun with `--code` (and the
    /// same `--verifier`) to exchange it.
    Oauth {
        /// `client-credentials` or `authorization-code`.
        #[arg(long, default_value = "client-credentials")]
        grant: String,

        #[arg(long)]
        client_id: String,

        #[arg(long)]
        client_secret: Option<String>,

        #[arg(long)]
        token_url: String,

        /// The browser-facing URL; required by the authorization-code grant.
        #[arg(long)]
        auth_url: Option<String>,

        #[arg(long)]
        redirect_uri: Option<String>,

        /// Repeatable: `--scope read`.
        #[arg(long = "scope")]
        scopes: Vec<String>,

        /// The authorization code returned on the redirect.
        #[arg(long)]
        code: Option<String>,

        /// The PKCE verifier printed by the first, code-less run.
        #[arg(long)]
        verifier: Option<String>,

        /// Do not use PKCE (the authorization-code grant only).
        #[arg(long = "no-pkce")]
        no_pkce: bool,
    },
}

struct SendArgs {
    url: String,
    method: String,
    headers: Vec<String>,
    json: Option<String>,
    insecure: bool,
    progress: bool,
    vars: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let store = JsonlStore::open(&cli.history)?;

    match cli.command {
        Command::Send {
            url,
            method,
            headers,
            json,
            insecure,
            progress,
            vars,
        } => {
            send(
                &store,
                SendArgs {
                    url,
                    method,
                    headers,
                    json,
                    insecure,
                    progress,
                    vars,
                },
            )
            .await
        }
        Command::History { query, limit } => {
            let hits = store.search_history(&query, limit)?;
            if hits.is_empty() {
                println!("no history yet ({})", store.path().display());
                return Ok(());
            }
            for hit in hits {
                println!(
                    "{:>4}  {:>3}  {}  {} {}",
                    hit.id, hit.status, hit.at, hit.method, hit.url
                );
            }
            Ok(())
        }
        Command::Import {
            file,
            format,
            verbose,
        } => {
            let import = read_import(&file, format.as_deref())?;
            println!("{:?}: {}", import.format, import.result.summary());
            if verbose {
                for collection in &import.result.collections {
                    println!("{}:", collection.name);
                    print_items(&collection.items, 1);
                }
            }
            for skip in &import.result.skipped {
                println!("skipped {}: {}", skip.at, skip.reason);
            }
            Ok(())
        }
        Command::Export {
            file,
            workspace,
            format,
        } => {
            let import = read_import(&file, format.as_deref())?;
            export_workspace(&workspace, import)
        }
        Command::Oauth {
            grant,
            client_id,
            client_secret,
            token_url,
            auth_url,
            redirect_uri,
            scopes,
            code,
            verifier,
            no_pkce,
        } => {
            oauth(
                &grant,
                client_id,
                client_secret,
                token_url,
                auth_url,
                redirect_uri,
                scopes,
                code,
                verifier,
                no_pkce,
            )
            .await
        }
    }
}

/// A parsed import plus the format that read it, so a caller can report both.
struct Parsed {
    format: ImportFormat,
    result: import::BulkImport,
}

/// Read `file` as a supported format.
fn read_import(file: &std::path::Path, format: Option<&str>) -> Result<Parsed> {
    let text = if file == std::path::Path::new("-") {
        let mut buffer = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)?;
        buffer
    } else {
        std::fs::read_to_string(file)?
    };

    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("import");

    let format = match format {
        Some(named) => match named.to_ascii_lowercase().as_str() {
            "curl" => ImportFormat::Curl,
            "postman" => ImportFormat::Postman,
            "har" => ImportFormat::Har,
            "openapi" => ImportFormat::OpenApi,
            "bundle" => ImportFormat::Bundle,
            other => return Err(Error::Other(format!("unknown format `{other}`"))),
        },
        None => ImportFormat::from_file_name(name).unwrap_or_else(|| ImportFormat::detect(&text)),
    };

    let result = import::parse(
        format,
        &text,
        Options {
            fallback_name: name,
            har_limit: None,
        },
    )?;

    Ok(Parsed { format, result })
}

/// Write a parsed import into a folder workspace, then report what landed.
fn export_workspace(workspace: &std::path::Path, import: Parsed) -> Result<()> {
    let store = FolderStore::open(workspace)?;
    for collection in &import.result.collections {
        store.save_collection(collection)?;
    }

    println!("{:?}: {}", import.format, import.result.summary());
    println!("workspace {}", store.root().display());
    for collection in &import.result.collections {
        println!("  wrote {}", collection.name);
    }
    for skip in &import.result.skipped {
        println!("  skipped {}: {}", skip.at, skip.reason);
    }
    Ok(())
}

fn print_items(items: &[restbook_core::CollectionItem], depth: usize) {
    let indent = "  ".repeat(depth);
    for item in items {
        match item {
            restbook_core::CollectionItem::Folder(folder) => {
                println!("{indent}{}:", folder.name);
                print_items(&folder.items, depth + 1);
            }
            restbook_core::CollectionItem::Request(saved) => println!(
                "{indent}{} {}  {}",
                saved.request.method, saved.name, saved.request.url
            ),
        }
    }
}

/// `/2048` when the total is known, so a progress line can show `512/2048`.
fn total_suffix(total: Option<u64>) -> String {
    match total {
        Some(total) => format!("/{total}"),
        None => String::new(),
    }
}

async fn send(store: &dyn Store, args: SendArgs) -> Result<()> {
    let mut context = VarContext::new();
    let mut scope = Vec::new();
    for pair in args.vars {
        match pair.split_once('=') {
            Some((key, value)) => scope.push((key.to_owned(), value.to_owned())),
            None => {
                return Err(Error::Other(format!(
                    "variable must be KEY=VALUE, got `{pair}`"
                )))
            }
        }
    }
    if !scope.is_empty() {
        context.push_scope(scope);
    }

    let method = Method::parse(&args.method)
        .ok_or_else(|| Error::Other(format!("unknown method `{}`", args.method)))?;

    let mut request = RequestDef::new(method, args.url);
    request.insecure = args.insecure;

    for header in args.headers {
        match header.split_once(':') {
            Some((key, value)) => request.headers.push(Kv::new(key.trim(), value.trim())),
            None => {
                return Err(Error::Other(format!(
                    "header must be `Key: value`, got `{header}`"
                )))
            }
        }
    }

    if let Some(text) = args.json {
        request.body = Body::Raw {
            mime: "application/json".to_owned(),
            text,
        };
    }

    let prepared = prepare(&request, &context)?;
    let transport = ReqwestTransport::new()?;
    let response = if args.progress {
        let progress = Progress::new();
        let watcher = progress.clone();
        // Poll the sink on a background task and print each change, so the send
        // path stays the only thing doing I/O.
        let printer = tokio::spawn(async move {
            let mut last = ProgressSnapshot::default();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                let now = watcher.snapshot();
                if now == last {
                    continue;
                }
                last = now;
                eprintln!(
                    "  [{:>5} ms] sent {}{} / received {}{}",
                    now.elapsed_ms,
                    now.sent_bytes,
                    total_suffix(now.sent_total),
                    now.received_bytes,
                    total_suffix(now.received_total),
                );
                if now.received_total == Some(now.received_bytes) && now.received_bytes > 0 {
                    break;
                }
            }
        });
        let result = transport
            .execute_with_progress(prepared.clone(), CancelToken::new(), progress)
            .await;
        printer.abort();
        result?
    } else {
        transport
            .execute(prepared.clone(), CancelToken::new())
            .await?
    };

    println!(
        "{} {} -> {} ({} ms, {} bytes)",
        request.method,
        prepared.url,
        response.status,
        response.duration_ms,
        response.body.len()
    );

    let text = response.text().to_string();
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
        Err(_) => println!("{text}"),
    }

    store.append_history(&HistoryEntry {
        id: 0,
        request,
        response,
        url: prepared.url.to_string(),
        // The CLI has no collections or drafts, so its sends are unowned.
        origin: Origin::default(),
    })?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn oauth(
    grant: &str,
    client_id: String,
    client_secret: Option<String>,
    token_url: String,
    auth_url: Option<String>,
    redirect_uri: Option<String>,
    scopes: Vec<String>,
    code: Option<String>,
    verifier: Option<String>,
    no_pkce: bool,
) -> Result<()> {
    let grant = match grant {
        "client-credentials" | "client_credentials" => OAuthGrant::ClientCredentials,
        "authorization-code" | "authorization_code" | "code" => OAuthGrant::AuthorizationCode,
        other => {
            return Err(Error::Other(format!(
                "unknown grant `{other}`: use client-credentials or authorization-code"
            )))
        }
    };
    let config = OAuthConfig {
        grant,
        client_id,
        client_secret: client_secret.unwrap_or_default(),
        auth_url: auth_url.unwrap_or_default(),
        token_url,
        scopes,
        redirect_uri: redirect_uri.unwrap_or_default(),
        pkce: !no_pkce,
    };

    let request = match grant {
        OAuthGrant::ClientCredentials => config.client_credentials_request()?,
        OAuthGrant::AuthorizationCode => match code {
            Some(code) => config.code_exchange_request(&code, verifier.as_deref().unwrap_or(""))?,
            None => {
                // No code yet: print what the browser needs and what to keep.
                let authorization = config.authorize()?;
                println!("open:               {}", authorization.url);
                println!("state (must match): {}", authorization.state);
                if config.pkce {
                    println!("verifier:           {}", authorization.verifier);
                }
                println!(
                    "then rerun with --code <code>{} and --redirect-uri <uri>",
                    if config.pkce {
                        " --verifier <verifier>"
                    } else {
                        ""
                    }
                );
                return Ok(());
            }
        },
    };

    let transport = ReqwestTransport::new()?;
    let token = transport.fetch_token(request).await?;

    println!("access_token: {}", token.access_token);
    if let Some(kind) = token.token_type {
        println!("token_type:   {kind}");
    }
    if let Some(expires_in) = token.expires_in {
        println!("expires_in:   {expires_in}s");
    }
    if let Some(scope) = token.scope {
        println!("scope:        {scope}");
    }
    if token.refresh_token.is_some() {
        println!("refresh_token: <received>");
    }

    Ok(())
}
