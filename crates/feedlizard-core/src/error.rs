use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    UnsupportedFeed,
    MalformedXml,
    MalformedJsonFeed,
    UnusableFeed,
    InvalidUrl,
    Opml(String),
    InputLimitExceeded(&'static str),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFeed => write!(f, "unsupported feed format"),
            Self::MalformedXml => write!(f, "malformed XML feed"),
            Self::MalformedJsonFeed => write!(f, "malformed JSON Feed"),
            Self::UnusableFeed => write!(f, "feed contains no usable metadata"),
            Self::InvalidUrl => write!(f, "invalid URL"),
            Self::Opml(message) => write!(f, "OPML error: {message}"),
            Self::InputLimitExceeded(limit) => write!(f, "input limit exceeded: {limit}"),
        }
    }
}

impl std::error::Error for CoreError {}
