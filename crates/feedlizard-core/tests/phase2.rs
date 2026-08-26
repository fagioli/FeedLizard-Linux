use feedlizard_core::{
    CoreError,
    discovery::{DiscoveryFormat, DiscoveryLink, rank_candidates},
    domain::{ArticleState, RETENTION_SECONDS, mark_all_as_read, should_expire},
    identity, ingestion, opml,
    parser::{self, FeedFormat, ImageSource},
};
use std::collections::HashSet;

const RDF: &str = include_str!("../../../fixtures/compatibility/rdf.xml");
const JSON: &str = include_str!("../../../fixtures/compatibility/json-feed.json");
const IMPERFECT: &str = include_str!("../../../fixtures/compatibility/malformed-usable.xml");
const OPML: &str = include_str!("../../../fixtures/compatibility/library.opml");

#[test]
fn parses_rdf_and_json_feed_metadata() {
    let rdf = parser::parse_with_source(RDF, "https://rdf.example/feed").unwrap();
    assert_eq!(rdf.format, FeedFormat::Rss);
    assert_eq!(rdf.title, "RDF News");
    assert_eq!(rdf.articles[0].author.as_deref(), Some("Editor"));
    assert_eq!(rdf.articles[0].updated_at, Some(1_787_659_200));

    let json = parser::parse_with_source(JSON, "https://json.example/feed.json").unwrap();
    assert_eq!(json.format, FeedFormat::Json);
    assert_eq!(json.articles.len(), 1); // malformed neighboring item is isolated
    assert_eq!(json.articles[0].author.as_deref(), Some("One, Two"));
    assert_eq!(
        json.articles[0].image.as_ref().unwrap().source,
        ImageSource::JsonImage
    );
    assert_eq!(json.icon_candidates, ["https://json.example/icon.png"]);
}

#[test]
fn malformed_optional_fields_do_not_discard_usable_articles() {
    let feed = parser::parse_with_source(IMPERFECT, "https://example.test/feed").unwrap();
    assert_eq!(feed.articles.len(), 2); // duplicate GUID suppressed
    assert_eq!(feed.articles[0].title, "First");
    assert_eq!(feed.articles[0].published_at, None);
    assert_eq!(feed.articles[0].summary.as_deref(), Some("Summary"));
    assert_eq!(
        feed.articles[0].content.as_deref(),
        Some("<p>Full</p><img src=\"/inline.jpg\" width=\"900\" height=\"500\">")
    );
    assert_eq!(
        feed.articles[0].image.as_ref().unwrap().url,
        "https://example.test/hero.jpg"
    );
    assert_eq!(feed.articles[1].title, "Untitled Article");
    assert_eq!(
        feed.articles[1].url.as_deref(),
        Some("https://example.test/relative")
    );
}

#[test]
fn parses_dates_deterministically() {
    let cases = [
        ("Tue, 25 Aug 2026 12:00:00 +0000", 1_787_659_200),
        ("2026-08-25T12:00:00Z", 1_787_659_200),
        ("2026-08-25T14:00:00+02:00", 1_787_659_200),
        ("2026-08-25T12:00:00.123Z", 1_787_659_200),
        ("2026-08-25", 1_787_616_000), // Apple-defined date-only behavior: UTC midnight
    ];
    for (value, expected) in cases {
        assert_eq!(parser::parse_date(value), Some(expected), "{value}");
    }
    assert_eq!(parser::parse_date("not a date"), None);
    assert_eq!(parser::parse_date("2026-08-25 12:00:00"), None);
}

#[test]
fn url_and_article_identity_edges_are_stable() {
    assert_eq!(
        identity::normalize_url("HTTPS://BÜCHER.example:443/feed/#x"),
        "https://xn--bcher-kva.example/feed"
    );
    assert_eq!(
        identity::normalize_url("https://example.com/feed/?a=1#x"),
        "https://example.com/feed?a=1"
    );
    assert_ne!(
        identity::feed_id("http://example.com/feed"),
        identity::feed_id("https://example.com/feed")
    );
    let feed = identity::feed_id("https://example.com/feed");
    assert_eq!(
        identity::article_id(&feed, Some("e\u{301}"), None, None, None),
        identity::article_id(&feed, Some("é"), None, None, None)
    );
    assert_eq!(
        identity::article_id(&feed, None, None, Some(" A   TITLE "), Some(10)),
        identity::article_id(&feed, None, None, Some("a title"), Some(10))
    );
}

#[test]
fn opml_round_trip_preserves_logical_library() {
    let first = opml::import(OPML).unwrap();
    assert_eq!(first.feeds.len(), 3);
    assert_eq!(first.feeds[0].folders, ["News & Analysis", "Technology"]);
    let xml = opml::export(&first, "Thu, 01 Jan 1970 00:00:00 +0000");
    let second = opml::import(&xml).unwrap();
    let logical = |library: &opml::OpmlLibrary| {
        let mut rows: Vec<_> = library
            .feeds
            .iter()
            .map(|f| {
                (
                    f.feed_url.clone(),
                    f.site_url.clone(),
                    f.title.clone(),
                    f.folders.clone(),
                    f.format,
                )
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    };
    assert_eq!(logical(&first), logical(&second));
}

#[test]
fn retention_and_local_state_transitions_match_policy() {
    let now = 2_000_000_000;
    assert!(!should_expire(
        false,
        Some(now - RETENTION_SECONDS),
        now,
        now
    ));
    assert!(should_expire(
        false,
        Some(now - RETENTION_SECONDS - 1),
        now,
        now
    ));
    assert!(!should_expire(true, Some(0), 0, now));
    assert!(should_expire(false, None, now - RETENTION_SECONDS - 1, now));
    let mut states = [
        ArticleState::default(),
        ArticleState {
            is_read: false,
            is_starred: true,
        },
    ];
    states[0].star();
    states[0].unstar();
    states[0].mark_unread();
    mark_all_as_read(&mut states);
    assert!(states.iter().all(|state| state.is_read));
    assert!(states[1].is_starred);
}

#[test]
fn ranks_offline_discovery_candidates_and_rejects_bad_schemes() {
    let links = vec![
        DiscoveryLink {
            href: "/rss".into(),
            mime_type: "application/rss+xml; charset=utf-8".into(),
            title: None,
        },
        DiscoveryLink {
            href: "/atom".into(),
            mime_type: "application/atom+xml".into(),
            title: Some("Atom".into()),
        },
        DiscoveryLink {
            href: "/rss".into(),
            mime_type: "application/rss+xml".into(),
            title: None,
        },
        DiscoveryLink {
            href: "javascript:bad".into(),
            mime_type: "application/feed+json".into(),
            title: None,
        },
    ];
    let result = rank_candidates("https://example.com/page", &links);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].format, DiscoveryFormat::Rss);
    assert_eq!(result[1].format, DiscoveryFormat::Atom);
}

#[test]
fn ingestion_rekeys_articles_and_reports_existing_identity() {
    let feed = parser::parse_with_source(JSON, "https://json.example/feed.json").unwrap();
    let first = ingestion::prepare(
        feed.clone(),
        "https://json.example/feed.json",
        &HashSet::new(),
        100,
    );
    let existing = HashSet::from([first.articles[0].stable_id.clone()]);
    let second = ingestion::prepare(feed, "https://json.example/feed.json", &existing, 200);
    assert_eq!(first.articles[0].stable_id, second.articles[0].stable_id);
    assert!(second.articles[0].is_existing);
}

#[test]
fn rejects_malformed_and_oversized_inputs() {
    assert_eq!(
        parser::parse("<rss><channel>"),
        Err(CoreError::MalformedXml)
    );
    assert_eq!(parser::parse("<html />"), Err(CoreError::UnsupportedFeed));
    let oversized = "x".repeat(parser::MAX_DOCUMENT_BYTES + 1);
    assert_eq!(
        parser::parse(&oversized),
        Err(CoreError::InputLimitExceeded("document bytes"))
    );
    assert!(opml::import("<rss />").is_err());
}

#[test]
fn parses_large_synthetic_feed_without_quadratic_state() {
    let items: String = (0..2_000)
        .map(|i| format!("<item><guid>{i}</guid><title>Story {i}</title></item>"))
        .collect();
    let xml = format!("<rss><channel><title>Large</title>{items}</channel></rss>");
    let feed = parser::parse(&xml).unwrap();
    assert_eq!(feed.articles.len(), 2_000);
}
