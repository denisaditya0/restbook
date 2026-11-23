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

    #[error("timed out: {0}")]
    Timeout(String),

    #[error("could not connect: {0}")]
    Connect(String),

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

/// Why a send failed, as the pane names it.
///
/// The message is for the details; the kind is what the UI acts on. It is set
/// once, at the boundary that produced the error, so no view has to match
/// English text to know whether a retry, a certificate decision, or a URL edit
/// is the next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Timeout,
    Connect,
    Tls,
    Proxy,
    InvalidUrl,
    Unresolved,
    Other,
}

impl FailureKind {
    pub fn title(self) -> &'static str {
        match self {
            FailureKind::Timeout => "Request timed out",
            FailureKind::Connect => "Couldn't connect",
            FailureKind::Tls => "Certificate not trusted",
            FailureKind::Proxy => "Proxy failed",
            FailureKind::InvalidUrl => "Invalid URL",
            FailureKind::Unresolved => "Unresolved variable",
            FailureKind::Other => "Request failed",
        }
    }
}

/// A failure as the app keeps it: a kind for the view, and the engine's message
/// for the details. `Clone` because every tab snapshot owns one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// A failure the engine did not classify, such as a store read.
    pub fn other(message: impl Into<String>) -> Self {
        Self::new(FailureKind::Other, message)
    }
}

impl Error {
    pub fn failure_kind(&self) -> FailureKind {
        match self {
            Error::UnknownVariable(_) | Error::UnknownGenerator(_) => FailureKind::Unresolved,
            Error::InvalidUrl(_) => FailureKind::InvalidUrl,
            Error::Tls(_) => FailureKind::Tls,
            Error::Proxy(_) => FailureKind::Proxy,
            Error::Timeout(_) => FailureKind::Timeout,
            Error::Connect(_) => FailureKind::Connect,
            _ => FailureKind::Other,
        }
    }

    pub fn failure(&self) -> Failure {
        Failure::new(self.failure_kind(), self.to_string())
    }
}

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
        let error = Error::UnknownVariable("missing".to_owned());
        // The message is rendered next to the field that failed, so it quotes
        // what the user typed rather than a single brace pair.
        assert_eq!(error.to_string(), "unknown variable `{{missing}}`");
        assert_eq!(error.failure_kind(), FailureKind::Unresolved);
    }

    #[test]
    fn a_wire_failure_carries_its_kind() {
        assert_eq!(
            Error::Timeout("operation timed out".to_owned()).failure_kind(),
            FailureKind::Timeout
        );
        assert_eq!(
            Error::Connect("Connection refused".to_owned()).failure_kind(),
            FailureKind::Connect
        );
        assert_eq!(
            Error::Tls("certificate".to_owned()).failure_kind(),
            FailureKind::Tls
        );
        let failure = Error::Connect("Connection refused".to_owned()).failure();
        assert_eq!(failure.kind.title(), "Couldn't connect");
    }
}
