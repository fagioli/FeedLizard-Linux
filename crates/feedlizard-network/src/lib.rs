mod discovery;
mod error;
mod transport;

pub use discovery::{DiscoveredFeed, DiscoveryResult};
pub use error::{NetworkError, NetworkErrorKind};
pub use transport::{
    CacheValidators, CancellationToken, FetchKind, FetchOutcome, FetchPolicy, FetchResponse,
    HttpClient,
};
