use feedlizard_core::{
    identity,
    parser::{self, FeedFormat},
};

const RSS: &str = include_str!("../../../fixtures/compatibility/rss.xml");
const ATOM: &str = include_str!("../../../fixtures/compatibility/atom.xml");
const IDENTITIES: &str = include_str!("../../../fixtures/compatibility/identity.tsv");

#[test]
fn parses_rss_contract() {
    let feed = parser::parse(RSS).unwrap();
    assert_eq!(feed.format, FeedFormat::Rss);
    assert_eq!(feed.title, "Example & News");
    assert_eq!(feed.site_url.as_deref(), Some("https://example.com/"));
    assert_eq!(feed.articles.len(), 2);
    assert_eq!(feed.articles[0].stable_id, "story-1");
    assert_eq!(feed.articles[0].author.as_deref(), Some("Ada Lovelace"));
    assert_eq!(feed.articles[1].title, "Untitled Article");
}

#[test]
fn parses_atom_contract() {
    let feed = parser::parse(ATOM).unwrap();
    assert_eq!(feed.format, FeedFormat::Atom);
    assert_eq!(feed.title, "Example Atom");
    assert_eq!(feed.site_url.as_deref(), Some("https://example.net/"));
    assert_eq!(feed.articles[0].stable_id, "urn:uuid:story-2");
    assert_eq!(feed.articles[0].author.as_deref(), Some("Grace Hopper"));
}

#[test]
fn normalizes_urls_like_feedlizard_v1() {
    assert_eq!(
        identity::normalize_url("HTTPS://Example.COM:443/feed/#section"),
        "https://example.com/feed"
    );
    assert_eq!(
        identity::normalize_url("http://Example.COM:80"),
        "http://example.com/"
    );
    assert_eq!(
        identity::normalize_url("https://Example.COM/feed/?page=1#x"),
        "https://example.com/feed?page=1"
    );
}

#[test]
fn identity_precedence_is_guid_then_url_then_fallback() {
    let feed = identity::feed_id("https://example.com/feed/");
    let guid = identity::article_id(
        &feed,
        Some("story-1"),
        Some("https://example.com/ignored"),
        Some("Ignored"),
        Some(1),
    );
    let url = identity::article_id(
        &feed,
        Some("null"),
        Some("HTTPS://EXAMPLE.COM/story/"),
        Some("Ignored"),
        Some(1),
    );
    let fallback = identity::article_id(
        &feed,
        None,
        None,
        Some("  A   Story  "),
        Some(1_700_000_000),
    );
    assert!(guid.starts_with("article:v1:"));
    assert_ne!(guid, url);
    assert_ne!(url, fallback);
    assert_eq!(guid.len(), 75);
}

#[test]
fn matches_cross_platform_identity_vectors() {
    let vectors: Vec<Vec<&str>> = IDENTITIES
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(identity::normalize_url(vectors[0][1]), vectors[0][2]);
    let feed = identity::feed_id(vectors[1][1]);
    assert_eq!(feed, vectors[1][2]);
    assert_eq!(
        identity::article_id(&feed, Some(vectors[2][1]), None, None, None),
        vectors[2][2]
    );
    assert_eq!(identity::feed_id(vectors[3][1]), vectors[3][2]);
}

#[test]
fn bleeping_computer_identity_is_apple_compatible() {
    assert_eq!(
        identity::normalize_url("https://www.bleepingcomputer.com/feed/"),
        "https://www.bleepingcomputer.com/feed"
    );
    assert_eq!(
        identity::feed_id("https://www.bleepingcomputer.com/feed/"),
        "feed:v1:c550f13a25b17b26fcdce9c39c9490ac5d66953e75b94abbea0413637e3ac4ff"
    );
}
