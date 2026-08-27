use crate::{DiscoveryResult, NetworkError, NetworkErrorKind, discovery};
use futures_util::StreamExt;
use reqwest::{StatusCode, Url, header};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

pub const DEFAULT_MAX_FEED_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_MAX_HTML_BYTES: usize = 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Default)]
pub struct CacheValidators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchKind {
    Feed,
    DiscoveryPage,
}

#[derive(Debug, Clone)]
pub struct FetchPolicy {
    pub request_timeout: Duration,
    pub max_feed_bytes: usize,
    pub max_html_bytes: usize,
    pub max_attempts: usize,
    pub max_retry_after: Duration,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
            max_feed_bytes: DEFAULT_MAX_FEED_BYTES,
            max_html_bytes: DEFAULT_MAX_HTML_BYTES,
            max_attempts: 2,
            max_retry_after: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FetchResponse {
    pub body: String,
    pub final_url: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub bytes_received: usize,
}

#[derive(Debug, Clone)]
pub enum FetchOutcome {
    Modified(FetchResponse),
    NotModified(FetchResponse),
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

#[derive(Debug, Default)]
struct CancellationInner {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }
    async fn cancelled(&self) {
        let notified = self.inner.notify.notified();
        if !self.is_cancelled() {
            notified.await;
        }
    }
}

#[derive(Clone)]
pub struct HttpClient {
    client: reqwest::Client,
    policy: FetchPolicy,
}

impl HttpClient {
    pub fn new(policy: FetchPolicy) -> Result<Self, NetworkError> {
        let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 8 {
                return attempt.error("redirect limit exceeded");
            }
            if !matches!(attempt.url().scheme(), "http" | "https") {
                return attempt.error("redirect target scheme is not HTTP/HTTPS");
            }
            if attempt.previous().first().is_some_and(|previous| {
                previous.scheme() == "https" && attempt.url().scheme() == "http"
            }) {
                return attempt.error("HTTPS downgrade redirect rejected");
            }
            attempt.follow()
        });
        let client = reqwest::Client::builder()
            .user_agent("FeedLizard-Linux/0.1 (+https://feedlizard.app)")
            .redirect(redirect_policy)
            .connect_timeout(Duration::from_secs(10))
            .timeout(policy.request_timeout)
            .build()
            .map_err(map_reqwest)?;
        Ok(Self { client, policy })
    }

    pub async fn fetch_feed(
        &self,
        url: &str,
        validators: &CacheValidators,
        cancel: &CancellationToken,
    ) -> Result<FetchOutcome, NetworkError> {
        self.fetch(url, FetchKind::Feed, validators, cancel).await
    }

    pub async fn discover(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<DiscoveryResult, NetworkError> {
        match self
            .fetch(
                url,
                FetchKind::DiscoveryPage,
                &CacheValidators::default(),
                cancel,
            )
            .await?
        {
            FetchOutcome::Modified(response) => {
                discovery::discover(&response.final_url, &response.body)
            }
            FetchOutcome::NotModified(_) => Err(NetworkError::new(
                NetworkErrorKind::InvalidResponse,
                "unexpected 304 discovery response",
            )),
        }
    }

    pub async fn discover_article_image(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, NetworkError> {
        match self
            .fetch(
                url,
                FetchKind::DiscoveryPage,
                &CacheValidators::default(),
                cancel,
            )
            .await?
        {
            FetchOutcome::Modified(response) => Ok(discovery::article_image(
                &response.final_url,
                &response.body,
            )),
            FetchOutcome::NotModified(_) => Ok(None),
        }
    }

    pub async fn discover_site_icon(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, NetworkError> {
        match self
            .fetch(
                url,
                FetchKind::DiscoveryPage,
                &CacheValidators::default(),
                cancel,
            )
            .await?
        {
            FetchOutcome::Modified(response) => {
                Ok(discovery::site_icon(&response.final_url, &response.body))
            }
            FetchOutcome::NotModified(_) => Ok(None),
        }
    }

    pub async fn fetch_article_html(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<FetchResponse, NetworkError> {
        match self
            .fetch(
                url,
                FetchKind::DiscoveryPage,
                &CacheValidators::default(),
                cancel,
            )
            .await?
        {
            FetchOutcome::Modified(response) => Ok(response),
            FetchOutcome::NotModified(_) => Err(NetworkError::new(
                NetworkErrorKind::InvalidResponse,
                "unexpected 304 article response",
            )),
        }
    }

    pub async fn fetch(
        &self,
        url: &str,
        kind: FetchKind,
        validators: &CacheValidators,
        cancel: &CancellationToken,
    ) -> Result<FetchOutcome, NetworkError> {
        validate_url(url)?;
        let attempts = self.policy.max_attempts.clamp(1, 3);
        let mut last = None;
        for attempt in 0..attempts {
            if cancel.is_cancelled() {
                return Err(NetworkError::new(
                    NetworkErrorKind::Cancelled,
                    "request cancelled",
                ));
            }
            match self.fetch_once(url, kind, validators, cancel).await {
                Ok(value) => return Ok(value),
                Err(error) if error.is_transient() && attempt + 1 < attempts => {
                    let delay = error
                        .retry_after
                        .unwrap_or(Duration::from_millis(150 * (attempt + 1) as u64))
                        .min(self.policy.max_retry_after);
                    tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = cancel.cancelled() => return Err(NetworkError::new(NetworkErrorKind::Cancelled, "request cancelled")) }
                    last = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last.unwrap_or_else(|| {
            NetworkError::new(NetworkErrorKind::InvalidResponse, "request failed")
        }))
    }

    async fn fetch_once(
        &self,
        url: &str,
        kind: FetchKind,
        validators: &CacheValidators,
        cancel: &CancellationToken,
    ) -> Result<FetchOutcome, NetworkError> {
        let mut request = self.client.get(url).header(header::ACCEPT, match kind { FetchKind::Feed => "application/rss+xml, application/atom+xml, application/feed+json, application/json;q=0.9, application/xml;q=0.8, text/xml;q=0.8, */*;q=0.1", FetchKind::DiscoveryPage => "text/html, application/xhtml+xml;q=0.9" });
        if let Some(value) = &validators.etag {
            request = request.header(header::IF_NONE_MATCH, value);
        }
        if let Some(value) = &validators.last_modified {
            request = request.header(header::IF_MODIFIED_SINCE, value);
        }
        let response = tokio::select! { value = request.send() => value.map_err(map_reqwest)?, _ = cancel.cancelled() => return Err(NetworkError::new(NetworkErrorKind::Cancelled, "request cancelled")) };
        validate_url(response.url().as_str())?;
        let status = response.status();
        let final_url = response.url().to_string();
        let etag = header_text(response.headers(), header::ETAG);
        let last_modified = header_text(response.headers(), header::LAST_MODIFIED);
        let content_type = header_text(response.headers(), header::CONTENT_TYPE);
        if status == StatusCode::NOT_MODIFIED {
            return Ok(FetchOutcome::NotModified(FetchResponse {
                body: String::new(),
                final_url,
                status: status.as_u16(),
                content_type,
                etag,
                last_modified,
                bytes_received: 0,
            }));
        }
        if !status.is_success() {
            return Err(status_error(status, response.headers()));
        }
        let limit = match kind {
            FetchKind::Feed => self.policy.max_feed_bytes,
            FetchKind::DiscoveryPage => self.policy.max_html_bytes,
        };
        if response
            .content_length()
            .is_some_and(|value| value > limit as u64)
        {
            return Err(NetworkError::new(
                NetworkErrorKind::OversizedResponse,
                "Content-Length exceeds response limit",
            ));
        }
        let header_bytes = response
            .headers()
            .iter()
            .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
            .sum::<usize>();
        if header_bytes > MAX_HEADER_BYTES {
            return Err(NetworkError::new(
                NetworkErrorKind::InvalidResponse,
                "response headers exceed limit",
            ));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        loop {
            let next = tokio::select! { value = stream.next() => value, _ = cancel.cancelled() => return Err(NetworkError::new(NetworkErrorKind::Cancelled, "request cancelled")) };
            let Some(chunk) = next else {
                break;
            };
            let chunk = chunk.map_err(map_reqwest)?;
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(NetworkError::new(
                    NetworkErrorKind::OversizedResponse,
                    "decoded response exceeds limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8(bytes).map_err(|_| {
            NetworkError::new(NetworkErrorKind::InvalidResponse, "response is not UTF-8")
        })?;
        validate_content(kind, content_type.as_deref(), &body)?;
        let bytes_received = body.len();
        Ok(FetchOutcome::Modified(FetchResponse {
            body,
            final_url,
            status: status.as_u16(),
            content_type,
            etag,
            last_modified,
            bytes_received,
        }))
    }
}

fn validate_url(value: &str) -> Result<Url, NetworkError> {
    let url = Url::parse(value)
        .map_err(|_| NetworkError::new(NetworkErrorKind::UnsupportedScheme, "invalid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(NetworkError::new(
            NetworkErrorKind::UnsupportedScheme,
            "only HTTP and HTTPS are supported",
        ));
    }
    if url.host_str().is_none() {
        return Err(NetworkError::new(
            NetworkErrorKind::UnsupportedScheme,
            "URL has no host",
        ));
    }
    Ok(url)
}

fn validate_content(
    kind: FetchKind,
    content_type: Option<&str>,
    body: &str,
) -> Result<(), NetworkError> {
    let mime = content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let trimmed = body.trim_start_matches('\u{feff}').trim_start();
    let prefix = trimmed
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    match kind {
        FetchKind::DiscoveryPage
            if mime == "text/html"
                || mime == "application/xhtml+xml"
                || trimmed.starts_with("<!doctype html")
                || trimmed.starts_with("<html") =>
        {
            Ok(())
        }
        FetchKind::DiscoveryPage => Err(NetworkError::new(
            NetworkErrorKind::UnsupportedContent,
            "response is not HTML",
        )),
        FetchKind::Feed
            if prefix.starts_with('{')
                || prefix.contains("<rss")
                || prefix.contains("<feed")
                || prefix.contains("<rdf:rdf") =>
        {
            Ok(())
        }
        FetchKind::Feed
            if matches!(
                mime.as_str(),
                "application/rss+xml"
                    | "application/atom+xml"
                    | "application/feed+json"
                    | "application/json"
                    | "application/xml"
                    | "text/xml"
            ) =>
        {
            Ok(())
        }
        FetchKind::Feed => Err(NetworkError::new(
            NetworkErrorKind::UnsupportedContent,
            "response does not look like a feed",
        )),
    }
}

fn header_text(headers: &header::HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn status_error(status: StatusCode, headers: &header::HeaderMap) -> NetworkError {
    let retry_after = headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after);
    let kind = match status.as_u16() {
        401 => NetworkErrorKind::Unauthorized,
        403 => NetworkErrorKind::Forbidden,
        404 => NetworkErrorKind::NotFound,
        410 => NetworkErrorKind::Gone,
        429 => NetworkErrorKind::RateLimited,
        500..=599 => NetworkErrorKind::Server,
        _ => NetworkErrorKind::InvalidResponse,
    };
    NetworkError::status(kind, status.as_u16(), retry_after)
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()?
                .duration_since(std::time::SystemTime::now())
                .ok()
        })
}

fn map_reqwest(error: reqwest::Error) -> NetworkError {
    let text = error.to_string();
    let lower = text.to_ascii_lowercase();
    let kind = if error.is_timeout() {
        NetworkErrorKind::Timeout
    } else if error.is_redirect() {
        NetworkErrorKind::Redirect
    } else if lower.contains("certificate") || lower.contains("tls") {
        NetworkErrorKind::Tls
    } else if error.is_connect() {
        NetworkErrorKind::Connectivity
    } else {
        NetworkErrorKind::InvalidResponse
    };
    NetworkError::new(kind, text)
}
