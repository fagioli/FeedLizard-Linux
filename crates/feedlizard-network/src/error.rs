use std::{error::Error, fmt, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkErrorKind {
    Cancelled,
    Timeout,
    Connectivity,
    Tls,
    Redirect,
    Unauthorized,
    Forbidden,
    NotFound,
    Gone,
    RateLimited,
    Server,
    InvalidResponse,
    OversizedResponse,
    UnsupportedContent,
    UnsupportedScheme,
}

#[derive(Debug, Clone)]
pub struct NetworkError {
    pub kind: NetworkErrorKind,
    pub status: Option<u16>,
    pub retry_after: Option<Duration>,
    pub detail: String,
}

impl NetworkError {
    pub(crate) fn new(kind: NetworkErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            retry_after: None,
            detail: detail.into(),
        }
    }

    pub fn new_for_refresh(kind: NetworkErrorKind, detail: impl Into<String>) -> Self {
        Self::new(kind, detail)
    }

    pub(crate) fn status(
        kind: NetworkErrorKind,
        status: u16,
        retry_after: Option<Duration>,
    ) -> Self {
        Self {
            kind,
            status: Some(status),
            retry_after,
            detail: format!("HTTP {status}"),
        }
    }

    pub fn is_transient(&self) -> bool {
        matches!(
            self.kind,
            NetworkErrorKind::Timeout
                | NetworkErrorKind::Connectivity
                | NetworkErrorKind::RateLimited
                | NetworkErrorKind::Server
        )
    }
}

impl fmt::Display for NetworkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.detail)
    }
}

impl Error for NetworkError {}
