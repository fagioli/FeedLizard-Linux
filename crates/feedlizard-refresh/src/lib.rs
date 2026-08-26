use feedlizard_core::parser;
use feedlizard_network::{
    CacheValidators, CancellationToken, DiscoveryResult, FetchOutcome, HttpClient, NetworkError,
    NetworkErrorKind,
};
use feedlizard_storage::{FeedRecord, IngestStats, Library, RefreshMetadata, StorageError};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

#[derive(Debug, Clone)]
pub struct RefreshConfig {
    pub global_concurrency: usize,
    pub per_host_concurrency: usize,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            global_concurrency: 6,
            per_host_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshState {
    Updated,
    Unchanged,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct RefreshResult {
    pub feed_id: String,
    pub state: RefreshState,
    pub status: Option<u16>,
    pub inserted: usize,
    pub updated: usize,
    pub bytes_received: usize,
    pub fetch_duration: Duration,
    pub parse_ingest_duration: Duration,
    pub failure: Option<RefreshFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshFailure {
    Network(NetworkErrorKind),
    Parse,
    Storage,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefreshSummary {
    pub total: usize,
    pub completed: usize,
    pub successful: usize,
    pub unchanged: usize,
    pub failed: usize,
    pub cancelled: usize,
}

#[derive(Debug, Clone)]
pub struct RefreshAllResult {
    pub summary: RefreshSummary,
    pub feeds: Vec<RefreshResult>,
}

#[derive(Debug)]
pub enum AddFeedResult {
    Added {
        feed_id: String,
        ingest: IngestStats,
    },
    Candidates(DiscoveryResult),
}

#[derive(Debug)]
pub enum RefreshError {
    Network(NetworkError),
    Storage(StorageError),
    Parse(String),
    InvalidUrl,
}

impl fmt::Display for RefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(e) => write!(f, "{e}"),
            Self::Storage(e) => write!(f, "{e}"),
            Self::Parse(e) => write!(f, "feed parse failed: {e}"),
            Self::InvalidUrl => write!(f, "invalid HTTP/HTTPS URL"),
        }
    }
}
impl Error for RefreshError {}
impl From<StorageError> for RefreshError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}
impl From<NetworkError> for RefreshError {
    fn from(value: NetworkError) -> Self {
        Self::Network(value)
    }
}

#[derive(Clone)]
pub struct RefreshCoordinator {
    client: HttpClient,
    global: Arc<Semaphore>,
    hosts: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
    per_host: usize,
}

impl RefreshCoordinator {
    pub fn new(client: HttpClient, config: RefreshConfig) -> Self {
        let global = config.global_concurrency.clamp(1, 32);
        Self {
            client,
            global: Arc::new(Semaphore::new(global)),
            hosts: Arc::new(Mutex::new(HashMap::new())),
            per_host: config.per_host_concurrency.clamp(1, global),
        }
    }

    pub async fn refresh_one(
        &self,
        library: &mut Library,
        feed_id: &str,
        cancel: &CancellationToken,
    ) -> Result<RefreshResult, RefreshError> {
        let feed = library.feed(feed_id)?;
        let fetched = self.fetch_subscription(feed.clone(), cancel.clone()).await;
        Ok(persist_result(library, feed, fetched))
    }

    pub async fn refresh_all(
        &self,
        library: &mut Library,
        cancel: &CancellationToken,
    ) -> Result<RefreshAllResult, RefreshError> {
        let feeds = library.list_feeds()?;
        let mut pending = FuturesUnordered::new();
        for feed in feeds {
            let coordinator = self.clone();
            let token = cancel.clone();
            pending.push(async move {
                let fetched = coordinator.fetch_subscription(feed.clone(), token).await;
                (feed, fetched)
            });
        }
        let mut results = Vec::new();
        while let Some((feed, fetched)) = pending.next().await {
            results.push(persist_result(library, feed, fetched));
        }
        results.sort_by(|a, b| a.feed_id.cmp(&b.feed_id));
        let mut summary = RefreshSummary {
            total: results.len(),
            completed: results.len(),
            ..RefreshSummary::default()
        };
        for result in &results {
            match result.state {
                RefreshState::Updated => summary.successful += 1,
                RefreshState::Unchanged => summary.unchanged += 1,
                RefreshState::Failed => summary.failed += 1,
                RefreshState::Cancelled => summary.cancelled += 1,
            }
        }
        Ok(RefreshAllResult {
            summary,
            feeds: results,
        })
    }

    pub async fn add_url(
        &self,
        library: &mut Library,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<AddFeedResult, RefreshError> {
        let parsed_url = Url::parse(url).map_err(|_| RefreshError::InvalidUrl)?;
        if !matches!(parsed_url.scheme(), "http" | "https") {
            return Err(RefreshError::InvalidUrl);
        }
        match self
            .client
            .fetch_feed(url, &CacheValidators::default(), cancel)
            .await
        {
            Ok(FetchOutcome::Modified(response)) => {
                parser::parse_with_source(&response.body, &response.final_url)
                    .map_err(|e| RefreshError::Parse(e.to_string()))?;
                let metadata = success_metadata(&response, now());
                let ingest = library.ingest_fetched_document(
                    url,
                    &response.final_url,
                    &response.body,
                    metadata.attempted_at,
                    Some(&metadata),
                )?;
                Ok(AddFeedResult::Added {
                    feed_id: feedlizard_core::identity::feed_id(url),
                    ingest,
                })
            }
            Ok(FetchOutcome::NotModified(_)) => Err(RefreshError::Parse(
                "unexpected 304 for new subscription".into(),
            )),
            Err(error) if error.kind == NetworkErrorKind::UnsupportedContent => Ok(
                AddFeedResult::Candidates(self.client.discover(url, cancel).await?),
            ),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn discover(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<DiscoveryResult, RefreshError> {
        Ok(self.client.discover(url, cancel).await?)
    }

    async fn fetch_subscription(
        &self,
        feed: FeedRecord,
        cancel: CancellationToken,
    ) -> FetchTaskResult {
        if cancel.is_cancelled() {
            return FetchTaskResult::Error(
                NetworkError::new_for_refresh(NetworkErrorKind::Cancelled, "refresh cancelled"),
                Duration::ZERO,
            );
        }
        let started = Instant::now();
        let global = tokio::select! { permit = self.global.clone().acquire_owned() => permit.ok(), _ = wait_cancelled(&cancel) => None };
        let Some(_global) = global else {
            return FetchTaskResult::Error(
                NetworkError::new_for_refresh(NetworkErrorKind::Cancelled, "refresh cancelled"),
                started.elapsed(),
            );
        };
        let host = Url::parse(&feed.fetch_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        let semaphore = {
            let mut hosts = self.hosts.lock().await;
            hosts
                .entry(host)
                .or_insert_with(|| Arc::new(Semaphore::new(self.per_host)))
                .clone()
        };
        let host_permit = tokio::select! { permit = semaphore.acquire_owned() => permit.ok(), _ = wait_cancelled(&cancel) => None };
        let Some(_host) = host_permit else {
            return FetchTaskResult::Error(
                NetworkError::new_for_refresh(NetworkErrorKind::Cancelled, "refresh cancelled"),
                started.elapsed(),
            );
        };
        let validators = CacheValidators {
            etag: feed.etag.clone(),
            last_modified: feed.last_modified.clone(),
        };
        match self
            .client
            .fetch_feed(&feed.fetch_url, &validators, &cancel)
            .await
        {
            Ok(value) => FetchTaskResult::Fetched(value, started.elapsed()),
            Err(error) => FetchTaskResult::Error(error, started.elapsed()),
        }
    }
}

enum FetchTaskResult {
    Fetched(FetchOutcome, Duration),
    Error(NetworkError, Duration),
}

struct Measurements {
    inserted: usize,
    updated: usize,
    bytes_received: usize,
    fetch_duration: Duration,
    parse_ingest_duration: Duration,
}

fn persist_result(
    library: &mut Library,
    feed: FeedRecord,
    fetched: FetchTaskResult,
) -> RefreshResult {
    let attempted_at = now();
    match fetched {
        FetchTaskResult::Fetched(FetchOutcome::NotModified(response), fetch_duration) => {
            let metadata = success_metadata(&response, attempted_at);
            match library.record_refresh(&feed.stable_id, &metadata) {
                Ok(()) => result(
                    &feed.stable_id,
                    RefreshState::Unchanged,
                    Some(response.status),
                    None,
                    Measurements {
                        inserted: 0,
                        updated: 0,
                        bytes_received: 0,
                        fetch_duration,
                        parse_ingest_duration: Duration::ZERO,
                    },
                ),
                Err(_) => result(
                    &feed.stable_id,
                    RefreshState::Failed,
                    Some(response.status),
                    Some(RefreshFailure::Storage),
                    Measurements {
                        inserted: 0,
                        updated: 0,
                        bytes_received: 0,
                        fetch_duration,
                        parse_ingest_duration: Duration::ZERO,
                    },
                ),
            }
        }
        FetchTaskResult::Fetched(FetchOutcome::Modified(response), fetch_duration) => {
            let work = Instant::now();
            if parser::parse_with_source(&response.body, &response.final_url).is_err() {
                let metadata = failure_metadata(attempted_at, Some(response.status), "parse");
                let _ = library.record_refresh(&feed.stable_id, &metadata);
                return result(
                    &feed.stable_id,
                    RefreshState::Failed,
                    Some(response.status),
                    Some(RefreshFailure::Parse),
                    Measurements {
                        inserted: 0,
                        updated: 0,
                        bytes_received: response.bytes_received,
                        fetch_duration,
                        parse_ingest_duration: work.elapsed(),
                    },
                );
            }
            let metadata = success_metadata(&response, attempted_at);
            match library.ingest_fetched_document(
                &feed.normalized_url,
                &response.final_url,
                &response.body,
                attempted_at,
                Some(&metadata),
            ) {
                Ok(stats) => result(
                    &feed.stable_id,
                    RefreshState::Updated,
                    Some(response.status),
                    None,
                    Measurements {
                        inserted: stats.inserted,
                        updated: stats.updated,
                        bytes_received: response.bytes_received,
                        fetch_duration,
                        parse_ingest_duration: work.elapsed(),
                    },
                ),
                Err(_) => result(
                    &feed.stable_id,
                    RefreshState::Failed,
                    Some(response.status),
                    Some(RefreshFailure::Storage),
                    Measurements {
                        inserted: 0,
                        updated: 0,
                        bytes_received: response.bytes_received,
                        fetch_duration,
                        parse_ingest_duration: work.elapsed(),
                    },
                ),
            }
        }
        FetchTaskResult::Error(error, fetch_duration) => {
            let category = format!("{:?}", error.kind).to_ascii_lowercase();
            let metadata = failure_metadata(attempted_at, error.status, &category);
            let _ = library.record_refresh(&feed.stable_id, &metadata);
            let state = if error.kind == NetworkErrorKind::Cancelled {
                RefreshState::Cancelled
            } else {
                RefreshState::Failed
            };
            result(
                &feed.stable_id,
                state,
                error.status,
                Some(RefreshFailure::Network(error.kind)),
                Measurements {
                    inserted: 0,
                    updated: 0,
                    bytes_received: 0,
                    fetch_duration,
                    parse_ingest_duration: Duration::ZERO,
                },
            )
        }
    }
}

fn success_metadata(response: &feedlizard_network::FetchResponse, now: i64) -> RefreshMetadata {
    RefreshMetadata {
        etag: response.etag.clone(),
        last_modified: response.last_modified.clone(),
        attempted_at: now,
        succeeded_at: Some(now),
        http_status: Some(response.status),
        failure_category: None,
        final_fetch_url: Some(response.final_url.clone()),
    }
}
fn failure_metadata(now: i64, status: Option<u16>, category: &str) -> RefreshMetadata {
    RefreshMetadata {
        etag: None,
        last_modified: None,
        attempted_at: now,
        succeeded_at: None,
        http_status: status,
        failure_category: Some(category.chars().take(64).collect()),
        final_fetch_url: None,
    }
}
fn result(
    feed_id: &str,
    state: RefreshState,
    status: Option<u16>,
    failure: Option<RefreshFailure>,
    measurements: Measurements,
) -> RefreshResult {
    RefreshResult {
        feed_id: feed_id.to_owned(),
        state,
        status,
        inserted: measurements.inserted,
        updated: measurements.updated,
        bytes_received: measurements.bytes_received,
        fetch_duration: measurements.fetch_duration,
        parse_ingest_duration: measurements.parse_ingest_duration,
        failure,
    }
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
async fn wait_cancelled(token: &CancellationToken) {
    while !token.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
