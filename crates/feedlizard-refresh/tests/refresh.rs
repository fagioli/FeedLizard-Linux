use feedlizard_core::{identity::feed_id, parser::FeedFormat};
use feedlizard_network::{CancellationToken, FetchPolicy, HttpClient};
use feedlizard_refresh::{RefreshConfig, RefreshCoordinator, RefreshFailure, RefreshState};
use feedlizard_storage::Library;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tempfile::TempDir;
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
}

async fn server(
    handler: impl Fn(Request) -> Response + Send + Sync + 'static,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handler = Arc::new(handler);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let handler = handler.clone();
            let active = active.clone();
            let peak = peak.clone();
            tokio::spawn(async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
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
                let reason = if response.status == 200 {
                    "OK"
                } else if response.status == 304 {
                    "Not Modified"
                } else {
                    "Error"
                };
                let mut head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    response.status,
                    reason,
                    response.body.len()
                );
                for (key, value) in response.headers {
                    head.push_str(&format!("{key}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(response.body.as_bytes()).await;
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    format!("http://{address}")
}

fn response(status: u16, mime: &str, body: String) -> Response {
    Response {
        status,
        headers: vec![("Content-Type".into(), mime.into())],
        body,
        delay: Duration::ZERO,
    }
}
fn rss(title: &str, count: usize) -> String {
    let items = (0..count).map(|index| format!("<item><guid>{title}-{index}</guid><title>{title} {index}</title><link>https://example.test/{title}/{index}</link></item>")).collect::<String>();
    format!(
        "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>{title}</title><link>https://example.test</link>{items}</channel></rss>"
    )
}
fn library() -> (TempDir, Library) {
    let directory = tempfile::tempdir().unwrap();
    let library = Library::open(directory.path().join("library.sqlite")).unwrap();
    (directory, library)
}
fn coordinator(global: usize, per_host: usize) -> RefreshCoordinator {
    RefreshCoordinator::new(
        HttpClient::new(FetchPolicy {
            max_attempts: 1,
            request_timeout: Duration::from_secs(2),
            ..FetchPolicy::default()
        })
        .unwrap(),
        RefreshConfig {
            global_concurrency: global,
            per_host_concurrency: per_host,
        },
    )
}

#[tokio::test]
async fn article_image_discovery_uses_bounded_webpage_metadata_path() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let base = server(
        |request| {
            assert_eq!(request.path, "/article");
            response(
                200,
                "text/html",
                r#"<html><head><meta property="og:image" content="/media/hero.jpg"></head></html>"#
                    .into(),
            )
        },
        active,
        peak,
    )
    .await;
    let service = coordinator(6, 2);
    let token = CancellationToken::default();
    let images = service
        .discover_article_images(
            vec![("article:v1:test".into(), format!("{base}/article"))],
            &token,
        )
        .await;
    assert_eq!(
        images,
        vec![("article:v1:test".into(), format!("{base}/media/hero.jpg"))]
    );
}

#[tokio::test]
async fn refresh_persists_validators_uses_304_and_preserves_articles() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let base = server(
        {
            let requests = requests.clone();
            move |request| {
                requests.lock().unwrap().push(request.headers.clone());
                if request
                    .headers
                    .get("if-none-match")
                    .is_some_and(|value| value == "\"one\"")
                {
                    return response(304, "application/rss+xml", String::new());
                }
                let mut value = response(200, "application/rss+xml", rss("Conditional", 3));
                value.headers.extend([
                    ("ETag".into(), "\"one\"".into()),
                    (
                        "Last-Modified".into(),
                        "Wed, 21 Oct 2015 07:28:00 GMT".into(),
                    ),
                ]);
                value
            }
        },
        active,
        peak,
    )
    .await;
    let (_directory, mut library) = library();
    let url = format!("{base}/feed");
    let id = library
        .add_subscription(&url, "Pending", FeedFormat::Rss, None, 1)
        .unwrap();
    let service = coordinator(6, 2);
    let token = CancellationToken::default();
    let first = service
        .refresh_one(&mut library, &id, &token)
        .await
        .unwrap();
    assert_eq!((first.state, first.inserted), (RefreshState::Updated, 3));
    let second = service
        .refresh_one(&mut library, &id, &token)
        .await
        .unwrap();
    assert_eq!(second.state, RefreshState::Unchanged);
    let feed = library.feed(&id).unwrap();
    assert_eq!(
        (
            feed.etag.as_deref(),
            feed.last_http_status,
            feed.consecutive_failures
        ),
        (Some("\"one\""), Some(304), 0)
    );
    assert_eq!(library.stats().unwrap().articles, 3);
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|headers| headers.contains_key("if-modified-since"))
    );
    library.integrity_check().unwrap();
}

#[tokio::test]
async fn atom_json_and_last_modified_paths_ingest_deterministically() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let base = server(
        |request| match request.path.as_str() {
            "/atom" => response(200, "application/atom+xml", r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"><title>Atom</title><id>urn:atom</id><entry><title>Entry</title><id>urn:entry</id><updated>2026-01-01T00:00:00Z</updated></entry></feed>"#.into()),
            "/json" => response(200, "application/feed+json", r#"{"version":"https://jsonfeed.org/version/1.1","title":"JSON","items":[{"id":"json-one","title":"JSON Entry"}]}"#.into()),
            "/modified" if request.headers.contains_key("if-modified-since") => response(304, "application/rss+xml", String::new()),
            "/modified" => { let mut value = response(200, "application/rss+xml", rss("Modified", 1)); value.headers.push(("Last-Modified".into(), "Wed, 21 Oct 2015 07:28:00 GMT".into())); value },
            _ => response(404, "text/plain", String::new()),
        },
        active,
        peak,
    ).await;
    let (_directory, mut library) = library();
    for (path, format) in [
        ("atom", FeedFormat::Atom),
        ("json", FeedFormat::Json),
        ("modified", FeedFormat::Rss),
    ] {
        library
            .add_subscription(&format!("{base}/{path}"), path, format, None, 1)
            .unwrap();
    }
    let service = coordinator(3, 2);
    let first = service
        .refresh_all(&mut library, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(
        (first.summary.successful, library.stats().unwrap().articles),
        (3, 3)
    );
    let modified_id = feed_id(&format!("{base}/modified"));
    let second = service
        .refresh_one(&mut library, &modified_id, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(second.state, RefreshState::Unchanged);
    library.integrity_check().unwrap();
}

#[tokio::test]
async fn refresh_all_isolates_failure_and_bounds_global_and_host_work() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let base = server(
        |request| {
            if request.path.contains("broken") {
                response(200, "application/rss+xml", "<rss><broken>".into())
            } else if request.path.contains("missing") {
                response(404, "text/plain", "missing".into())
            } else {
                let mut value = response(
                    200,
                    "application/rss+xml",
                    rss(request.path.trim_start_matches('/'), 2),
                );
                value.delay = Duration::from_millis(20);
                value
            }
        },
        active,
        peak.clone(),
    )
    .await;
    let (_directory, mut library) = library();
    for index in 0..8 {
        library
            .add_subscription(
                &format!("{base}/feed-{index}"),
                &format!("Feed {index}"),
                FeedFormat::Rss,
                None,
                1,
            )
            .unwrap();
    }
    library
        .add_subscription(
            &format!("{base}/broken"),
            "Broken",
            FeedFormat::Rss,
            None,
            1,
        )
        .unwrap();
    library
        .add_subscription(
            &format!("{base}/missing"),
            "Missing",
            FeedFormat::Rss,
            None,
            1,
        )
        .unwrap();
    let result = coordinator(4, 2)
        .refresh_all(&mut library, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(
        (
            result.summary.total,
            result.summary.successful,
            result.summary.failed
        ),
        (10, 8, 2)
    );
    assert!(peak.load(Ordering::SeqCst) <= 2);
    assert_eq!(library.stats().unwrap().articles, 16);
    assert!(
        result
            .feeds
            .iter()
            .any(|value| value.failure == Some(RefreshFailure::Parse))
    );
    assert!(result.feeds.iter().any(|value| value.failure
        == Some(RefreshFailure::Network(
            feedlizard_network::NetworkErrorKind::NotFound
        ))));
    library.integrity_check().unwrap();
}

#[tokio::test]
async fn cancellation_and_offline_failure_preserve_local_state() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let base = server(
        |_| {
            let mut value = response(200, "application/rss+xml", rss("Slow", 1));
            value.delay = Duration::from_secs(1);
            value
        },
        active,
        peak,
    )
    .await;
    let (_directory, mut library) = library();
    let id = library
        .add_subscription(&format!("{base}/slow"), "Slow", FeedFormat::Rss, None, 1)
        .unwrap();
    let token = CancellationToken::default();
    let trigger = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(25)).await;
        trigger.cancel();
    });
    let cancelled = coordinator(2, 1)
        .refresh_one(&mut library, &id, &token)
        .await
        .unwrap();
    assert_eq!(cancelled.state, RefreshState::Cancelled);
    assert_eq!(library.stats().unwrap().articles, 0);
    let offline_url = "http://127.0.0.1:1/offline";
    let offline_id = library
        .add_subscription(offline_url, "Offline", FeedFormat::Rss, None, 1)
        .unwrap();
    let failed = coordinator(2, 1)
        .refresh_one(&mut library, &offline_id, &CancellationToken::default())
        .await
        .unwrap();
    assert_eq!(failed.state, RefreshState::Failed);
    assert_eq!(library.stats().unwrap().articles, 0);
}

#[tokio::test]
async fn add_feed_and_discovery_use_production_paths() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let base_cell = Arc::new(Mutex::new(String::new()));
    let base_for_handler = base_cell.clone();
    let base = server(move |request| match request.path.as_str() {
        "/feed" => response(200, "application/rss+xml", rss("Added", 4)),
        "/dated" => response(200, "application/rss+xml", "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>Dated</title><link>https://example.test</link><item><guid>old</guid><title>Old</title><pubDate>Wed, 21 Oct 2015 07:28:00 GMT</pubDate></item><item><guid>current</guid><title>Current</title></item></channel></rss>".into()),
        "/page" => response(200, "text/html", r#"<!doctype html><html><head><link rel="alternate" type="application/rss+xml" href="/feed" title="Main"></head></html>"#.into()),
        _ => response(404, "text/plain", base_for_handler.lock().unwrap().clone()),
    }, active, peak).await;
    *base_cell.lock().unwrap() = base.clone();
    let (_directory, mut library) = library();
    let service = coordinator(2, 1);
    let token = CancellationToken::default();
    let added = service
        .add_url(&mut library, &format!("{base}/feed"), &token)
        .await
        .unwrap();
    assert!(matches!(
        added,
        feedlizard_refresh::AddFeedResult::Added { .. }
    ));
    assert_eq!(library.stats().unwrap().articles, 4);
    let dated = service
        .add_url(&mut library, &format!("{base}/dated"), &token)
        .await
        .unwrap();
    let feedlizard_refresh::AddFeedResult::Added { ingest, .. } = dated else {
        panic!()
    };
    assert_eq!(ingest.inserted, 1);
    assert_eq!(library.stats().unwrap().articles, 5);
    let discovered = service
        .add_url(&mut library, &format!("{base}/page"), &token)
        .await
        .unwrap();
    let feedlizard_refresh::AddFeedResult::Candidates(candidates) = discovered else {
        panic!()
    };
    assert_eq!(candidates.candidates[0].url, format!("{base}/feed"));
}

#[tokio::test]
#[ignore = "explicit Phase 4 local refresh measurement"]
async fn deterministic_refresh_measurements() {
    for total in [1usize, 10, 50, 100] {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let base = server(
            |request| {
                if request.headers.contains_key("if-none-match") {
                    return response(304, "application/rss+xml", String::new());
                }
                let mut value = response(
                    200,
                    "application/rss+xml",
                    rss(request.path.trim_start_matches('/'), 20),
                );
                value.headers.push(("ETag".into(), "\"benchmark\"".into()));
                value
            },
            active,
            peak.clone(),
        )
        .await;
        let (_directory, mut library) = library();
        for index in 0..total {
            library
                .add_subscription(
                    &format!("{base}/feed-{index}"),
                    &format!("Feed {index}"),
                    FeedFormat::Rss,
                    None,
                    1,
                )
                .unwrap();
        }
        let service = coordinator(6, 2);
        let started = Instant::now();
        let result = service
            .refresh_all(&mut library, &CancellationToken::default())
            .await
            .unwrap();
        let updated_ms = started.elapsed().as_secs_f64() * 1000.0;
        let unchanged_started = Instant::now();
        let unchanged = service
            .refresh_all(&mut library, &CancellationToken::default())
            .await
            .unwrap();
        println!(
            "feeds={total} articles={} first_refresh_ms={updated_ms:.3} not_modified_refresh_ms={:.3} peak_host_concurrency={} successful={} unchanged={}",
            library.stats().unwrap().articles,
            unchanged_started.elapsed().as_secs_f64() * 1000.0,
            peak.load(Ordering::SeqCst),
            result.summary.successful,
            unchanged.summary.unchanged
        );
        library.integrity_check().unwrap();
    }
}

#[test]
fn bleepingcomputer_identity_is_unchanged() {
    assert_eq!(
        feed_id("https://www.bleepingcomputer.com/feed/"),
        "feed:v1:c550f13a25b17b26fcdce9c39c9490ac5d66953e75b94abbea0413637e3ac4ff"
    );
}
