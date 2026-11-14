//! Development harness for the restbook engine.
//!
//! This is not a product feature. It exists so the request pipeline can be
//! exercised, debugged, and regression-checked without a window.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use restbook_core::{
    prepare, Body, CancelToken, Error, HistoryEntry, Kv, Method, RequestDef, Result, Store,
    Transport, VarContext,
};
use restbook_store::JsonlStore;
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
}

struct SendArgs {
    url: String,
    method: String,
    headers: Vec<String>,
    json: Option<String>,
    insecure: bool,
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
    let response = transport.execute(prepared.clone(), CancelToken::new()).await?;

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
    })?;

    Ok(())
}
