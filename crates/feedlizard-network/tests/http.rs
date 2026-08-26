use feedlizard_network::{
    CacheValidators, CancellationToken, FetchKind, FetchOutcome, FetchPolicy, HttpClient,
    NetworkErrorKind,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[derive(Clone)]
struct Request {
    path: String,
    headers: HashMap<String, String>,
}
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
    delay: Duration,
    truncate: bool,
}

async fn server(handler: impl Fn(Request) -> Response + Send + Sync + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let handler = handler.clone();
            tokio::spawn(async move {
                let mut bytes = vec![0; 16 * 1024];
                let count = socket.read(&mut bytes).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&bytes[..count]);
                let mut lines = text.lines();
                let path = lines
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let headers = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
                    .collect();
                let response = handler(Request { path, headers });
                tokio::time::sleep(response.delay).await;
                let reason = match response.status {
                    200 => "OK",
                    302 => "Found",
                    304 => "Not Modified",
                    404 => "Not Found",
                    410 => "Gone",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    503 => "Service Unavailable",
                    _ => "Response",
                };
                let declared = if response.truncate {
                    response.body.len() + 100
                } else {
                    response.body.len()
                };
                let mut head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    response.status, reason, declared
                );
                for (key, value) in response.headers {
                    head.push_str(&format!("{key}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(response.body.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

fn response(status: u16, content_type: &str, body: &str) -> Response {
    Response {
        status,
        headers: vec![("Content-Type".into(), content_type.into())],
        body: body.into(),
        delay: Duration::ZERO,
        truncate: false,
    }
}
fn rss() -> &'static str {
    r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Test</title><link>https://example.test</link><item><guid>one</guid><title>One</title></item></channel></rss>"#
}

#[tokio::test]
async fn deterministic_http_policy_matrix() {
    let retry_count = Arc::new(AtomicUsize::new(0));
    let seen_headers = Arc::new(Mutex::new(Vec::new()));
    let base = server({
        let retry_count = retry_count.clone();
        let seen_headers = seen_headers.clone();
        move |request| {
            seen_headers.lock().unwrap().push(request.headers.clone());
            match request.path.as_str() {
                "/rss" => { let mut value = response(200, "text/plain", rss()); value.headers.extend([("ETag".into(), "\"v1\"".into()), ("Last-Modified".into(), "Wed, 21 Oct 2015 07:28:00 GMT".into())]); value }
                "/conditional" if request.headers.get("if-none-match").is_some_and(|value| value == "\"v1\"") => response(304, "application/rss+xml", ""),
                "/conditional" => { let mut value = response(200, "application/rss+xml", rss()); value.headers.push(("ETag".into(), "\"v1\"".into())); value }
                "/redirect" => { let mut value = response(302, "text/plain", ""); value.headers.push(("Location".into(), "/rss".into())); value }
                "/loop" => { let mut value = response(302, "text/plain", ""); value.headers.push(("Location".into(), "/loop".into())); value }
                "/404" => response(404, "text/plain", "missing"),
                "/410" => response(410, "text/plain", "gone"),
                "/429" => { retry_count.fetch_add(1, Ordering::SeqCst); let mut value = response(429, "text/plain", "later"); value.headers.push(("Retry-After".into(), "0".into())); value }
                "/503" => { retry_count.fetch_add(1, Ordering::SeqCst); response(503, "text/plain", "later") }
                "/html" => response(200, "text/html", r#"<!doctype html><html><head><link rel="alternate" type="application/rss+xml" href="/rss" title="RSS"><link rel="alternate" type="application/atom+xml" href="/atom"><link rel="alternate" type="application/rss+xml" href="/rss"></head></html>"#),
                "/oversize" => response(200, "application/rss+xml", &"x".repeat(2048)),
                "/truncated" => { let mut value = response(200, "application/rss+xml", rss()); value.truncate = true; value }
                _ => response(500, "text/plain", "bad route"),
            }
        }
    }).await;
    let policy = FetchPolicy {
        max_feed_bytes: 1024,
        max_attempts: 2,
        max_retry_after: Duration::ZERO,
        ..FetchPolicy::default()
    };
    let client = HttpClient::new(policy).unwrap();
    let token = CancellationToken::default();

    let FetchOutcome::Modified(first) = client
        .fetch_feed(&format!("{base}/rss"), &CacheValidators::default(), &token)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (first.etag.as_deref(), first.last_modified.as_deref()),
        (Some("\"v1\""), Some("Wed, 21 Oct 2015 07:28:00 GMT"))
    );
    let validators = CacheValidators {
        etag: first.etag,
        last_modified: first.last_modified,
    };
    assert!(matches!(
        client
            .fetch_feed(&format!("{base}/conditional"), &validators, &token)
            .await
            .unwrap(),
        FetchOutcome::NotModified(_)
    ));
    let FetchOutcome::Modified(redirected) = client
        .fetch_feed(
            &format!("{base}/redirect"),
            &CacheValidators::default(),
            &token,
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(redirected.final_url.ends_with("/rss"));
    assert_eq!(
        client
            .fetch_feed(&format!("{base}/loop"), &CacheValidators::default(), &token)
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::Redirect
    );
    assert_eq!(
        client
            .fetch_feed(&format!("{base}/404"), &CacheValidators::default(), &token)
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::NotFound
    );
    assert_eq!(
        client
            .fetch_feed(&format!("{base}/410"), &CacheValidators::default(), &token)
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::Gone
    );
    assert_eq!(
        client
            .fetch_feed(&format!("{base}/429"), &CacheValidators::default(), &token)
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::RateLimited
    );
    assert_eq!(
        client
            .fetch_feed(&format!("{base}/503"), &CacheValidators::default(), &token)
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::Server
    );
    assert_eq!(retry_count.load(Ordering::SeqCst), 4);
    assert_eq!(
        client
            .fetch_feed(
                &format!("{base}/oversize"),
                &CacheValidators::default(),
                &token
            )
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::OversizedResponse
    );
    assert_eq!(
        client
            .fetch_feed(
                &format!("{base}/truncated"),
                &CacheValidators::default(),
                &token
            )
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::InvalidResponse
    );
    let discovery = client
        .discover(&format!("{base}/html"), &token)
        .await
        .unwrap();
    assert_eq!(discovery.candidates.len(), 2);
    assert_eq!(discovery.candidates[0].url, format!("{base}/rss"));
    let headers = seen_headers.lock().unwrap();
    assert!(headers.iter().any(|value| {
        value
            .get("user-agent")
            .is_some_and(|agent| agent.starts_with("FeedLizard-Linux/"))
    }));
    assert!(headers.iter().any(|value| value.contains_key("accept")));
}

#[tokio::test]
async fn response_stream_limit_and_cancellation_are_enforced() {
    let base = server(|request| match request.path.as_str() {
        "/stream" => response(200, "application/rss+xml", &"x".repeat(2048)),
        "/slow" => {
            let mut value = response(200, "application/rss+xml", rss());
            value.delay = Duration::from_secs(2);
            value
        }
        _ => response(404, "text/plain", ""),
    })
    .await;
    let client = HttpClient::new(FetchPolicy {
        max_feed_bytes: 512,
        max_attempts: 1,
        ..FetchPolicy::default()
    })
    .unwrap();
    let token = CancellationToken::default();
    assert_eq!(
        client
            .fetch(
                &format!("{base}/stream"),
                FetchKind::Feed,
                &CacheValidators::default(),
                &token
            )
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::OversizedResponse
    );
    let cancellation = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(25)).await;
        cancellation.cancel();
    });
    assert_eq!(
        client
            .fetch_feed(&format!("{base}/slow"), &CacheValidators::default(), &token)
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::Cancelled
    );

    let timeout_client = HttpClient::new(FetchPolicy {
        request_timeout: Duration::from_millis(20),
        max_attempts: 1,
        ..FetchPolicy::default()
    })
    .unwrap();
    assert_eq!(
        timeout_client
            .fetch_feed(
                &format!("{base}/slow"),
                &CacheValidators::default(),
                &CancellationToken::default(),
            )
            .await
            .unwrap_err()
            .kind,
        NetworkErrorKind::Timeout
    );
}

#[tokio::test]
async fn schemes_are_restricted_but_localhost_is_allowed() {
    let client = HttpClient::new(FetchPolicy {
        max_attempts: 1,
        ..FetchPolicy::default()
    })
    .unwrap();
    let token = CancellationToken::default();
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/plain,x",
    ] {
        assert_eq!(
            client
                .fetch_feed(url, &CacheValidators::default(), &token)
                .await
                .unwrap_err()
                .kind,
            NetworkErrorKind::UnsupportedScheme
        );
    }
}
