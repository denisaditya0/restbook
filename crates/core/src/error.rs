use thiserror::Error;

/// Everything the engine can fail with. The UI is expected to render these
/// directly, so messages are written for humans.
#[derive(Debug, Error)]
pub enum Error {
    #[error("unknown variable `{{{{{0}}}}}`")]
    UnknownVariable(String),

    #[error("unknown generator `{{{{{0}}}}}`")]
    UnknownGenerator(String),

    #[error("invalid url: {0}")]
    InvalidUrl(String),

    #[error("tls: {0}")]
    Tls(String),

    #[error("proxy: {0}")]
    Proxy(String),

    #[error("invalid header: {0}")]
    InvalidHeader(String),

    #[error("request cancelled")]
    Cancelled,

    #[error("transport error: {0}")]
    Transport(String),

    #[error("oauth: {0}")]
    OAuth(String),

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_errors_name_the_token_in_braces() {
        let error = Error::UnknownGenerator("$nope".to_owned());
        assert_eq!(error.to_string(), "unknown generator `{{$nope}}`");
    }

    #[test]
    fn variable_errors_read_the_same_way() {
        // The message is rendered next to the field that failed, so it quotes
        // what the user typed rather than a single brace pair.
        let error = Error::UnknownVariable("missing".to_owned());
        assert_eq!(error.to_string(), "unknown variable `{{missing}}`");
    }
}
