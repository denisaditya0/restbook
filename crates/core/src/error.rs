use thiserror::Error;

/// Everything the engine can fail with. The UI is expected to render these
/// directly, so messages are written for humans.
#[derive(Debug, Error)]
pub enum Error {
    #[error("unknown variable `{{{0}}}`")]
    UnknownVariable(String),

    #[error("invalid url: {0}")]
    InvalidUrl(String),

    #[error("invalid header: {0}")]
    InvalidHeader(String),

    #[error("request cancelled")]
    Cancelled,

    #[error("transport error: {0}")]
    Transport(String),

    #[error("store error: {0}")]
    Store(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
