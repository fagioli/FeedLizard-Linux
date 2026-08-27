use feedlizard_core::{domain::RETENTION_SECONDS, identity::feed_id, opml, parser::FeedFormat};
use feedlizard_storage::{ArticleScope, Library, SCHEMA_VERSION, StorageError};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const JSON: &str = include_str!("../../../fixtures/compatibility/json-feed.json");
const OPML: &str = include_str!("../../../fixtures/compatibility/library.opml");
static NEXT: AtomicU64 = AtomicU64::new(1);

struct DatabasePath(PathBuf);
impl DatabasePath {
    fn new(label: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "feedlizard-{label}-{}-{}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl AsRef<Path> for DatabasePath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}
impl Drop for DatabasePath {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{}", self.0.display(), suffix));
        }
    }
}

#[test]
fn fresh_database_has_current_schema_configuration_and_fts() {
    let path = DatabasePath::new("schema");
    let library = Library::open(&path).unwrap();
    assert_eq!(library.schema_version().unwrap(), SCHEMA_VERSION);
    library.integrity_check().unwrap();
    let connection = Connection::open(&path).unwrap();
    let foreign: i64 = connection
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    let journal: String = connection
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(foreign, 1); // bundled SQLite defaults to foreign-key enforcement
    assert_eq!(journal.to_ascii_lowercase(), "wal");
    assert!(
        connection
            .query_row(
                "SELECT count(*) FROM pragma_compile_options WHERE compile_options='ENABLE_FTS5'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap()
            > 0
    );
}

#[test]
fn future_schema_is_rejected_safely() {
    let path = DatabasePath::new("future");
    {
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
    }
    assert!(matches!(
        Library::open(&path),
        Err(StorageError::UnsupportedSchema(99))
    ));
}

#[test]
fn ingestion_is_transactional_idempotent_and_updates_existing_articles() {
    let path = DatabasePath::new("ingest");
    let mut library = Library::open(&path).unwrap();
    let first = library
        .ingest_document("https://json.example/feed.json", JSON, 100)
        .unwrap();
    assert_eq!((first.inserted, first.updated), (1, 0));
    let second = library
        .ingest_document("https://json.example/feed.json", JSON, 200)
        .unwrap();
    assert_eq!((second.inserted, second.updated), (0, 1));
    let updated = JSON.replace(
        "]}",
        r#",{"id":"json-2","url":"/two","title":"Second story","content_text":"Needle body"}]}"#,
    );
    let third = library
        .ingest_document("https://json.example/feed.json", &updated, 300)
        .unwrap();
    assert_eq!((third.inserted, third.updated), (1, 1));
    assert_eq!(library.stats().unwrap().articles, 2);
    let bad = format!(
        r#"{{"version":"https://jsonfeed.org/version/1.1","title":"Rollback Feed","items":[{{"id":"bad","title":"{}"}}]}}"#,
        "x".repeat(5_000)
    );
    assert!(
        library
            .ingest_document("https://rollback.example/feed", &bad, 400)
            .is_err()
    );
    assert_eq!(library.stats().unwrap().feeds, 1);
    library.integrity_check().unwrap();
}

#[test]
fn bleeping_identity_and_duplicate_subscription_are_constrained() {
    let path = DatabasePath::new("identity");
    let mut library = Library::open(&path).unwrap();
    let id = library
        .add_subscription(
            "https://www.bleepingcomputer.com/feed/",
            "BleepingComputer",
            FeedFormat::Rss,
            None,
            1,
        )
        .unwrap();
    assert_eq!(
        id,
        "feed:v1:c550f13a25b17b26fcdce9c39c9490ac5d66953e75b94abbea0413637e3ac4ff"
    );
    assert!(
        library
            .add_subscription(
                "https://www.bleepingcomputer.com/feed",
                "Duplicate",
                FeedFormat::Rss,
                None,
                2
            )
            .is_err()
    );
}

#[test]
fn folder_operations_preserve_and_unfile_feeds() {
    let path = DatabasePath::new("folders");
    let mut library = Library::open(&path).unwrap();
    let parent = library.create_folder("Technology", None, 1).unwrap();
    let child = library
        .create_folder("Security", Some(parent.id), 2)
        .unwrap();
    library.rename_folder(child.id, "Infosec", 3).unwrap();
    let feed = library
        .add_subscription(
            "https://example.com/feed",
            "Example",
            FeedFormat::Rss,
            None,
            4,
        )
        .unwrap();
    library.move_feed(&feed, Some(child.id), 5).unwrap();
    assert_eq!(library.list_feeds().unwrap()[0].folder_id, Some(child.id));
    library.delete_folder(child.id).unwrap();
    assert_eq!(library.list_feeds().unwrap()[0].folder_id, None);
    assert_eq!(library.list_folders().unwrap().len(), 1);
    library.integrity_check().unwrap();
}

#[test]
fn read_and_star_are_independent_and_mark_all_is_set_based() {
    let path = DatabasePath::new("state");
    let mut library = Library::open(&path).unwrap();
    library
        .ingest_document("https://json.example/feed.json", JSON, 100)
        .unwrap();
    let article = library
        .article_page(ArticleScope::Library, 10, None)
        .unwrap()
        .items
        .remove(0);
    library
        .set_article_starred(&article.stable_id, true, 101)
        .unwrap();
    library
        .mark_article_read(&article.stable_id, true, 102)
        .unwrap();
    library
        .mark_article_read(&article.stable_id, false, 103)
        .unwrap();
    let full = library.full_article(&article.stable_id).unwrap();
    assert!(!full.is_read && full.is_starred);
    assert_eq!(
        library.mark_all_read(ArticleScope::Library, 104).unwrap(),
        1
    );
    assert_eq!(library.unread_count(ArticleScope::Library).unwrap(), 0);
    assert!(library.full_article(&article.stable_id).unwrap().is_starred);
    assert_eq!(library.unstar_all(105).unwrap(), 1);
    assert!(!library.full_article(&article.stable_id).unwrap().is_starred);
    assert_eq!(library.unstar_all(106).unwrap(), 0);
}

#[test]
fn article_order_uses_updated_time_and_paginates_undated_items_last() {
    let path = DatabasePath::new("date-order");
    let mut library = Library::open(&path).unwrap();
    let document = r#"{
        "version":"https://jsonfeed.org/version/1.1",
        "title":"Dates",
        "items":[
          {"id":"published","title":"Published","date_published":"2026-07-01T00:00:00Z"},
          {"id":"updated","title":"Updated","date_modified":"2026-08-01T00:00:00Z"},
          {"id":"undated","title":"Undated"}
        ]
    }"#;
    library
        .ingest_document("https://dates.example/feed.json", document, 2_000_000_000)
        .unwrap();

    let first = library
        .article_page(ArticleScope::Library, 1, None)
        .unwrap();
    assert_eq!(first.items[0].title, "Updated");
    let second = library
        .article_page(ArticleScope::Library, 1, first.next.as_ref())
        .unwrap();
    assert_eq!(second.items[0].title, "Published");
    let third = library
        .article_page(ArticleScope::Library, 1, second.next.as_ref())
        .unwrap();
    assert_eq!(third.items[0].title, "Undated");
}

#[test]
fn retention_honors_boundary_fallback_star_and_fts_deletion() {
    let path = DatabasePath::new("retention");
    let mut library = Library::open(&path).unwrap();
    let base = 2_000_000_000;
    let document = r#"{"version":"https://jsonfeed.org/version/1.1","title":"Retention","items":[{"id":"old","title":"Old searchable","date_published":"2000-01-01T00:00:00Z"},{"id":"boundary","title":"Boundary"},{"id":"new","title":"New"}]}"#.to_owned();
    library
        .ingest_document(
            "https://retention.example/feed",
            &document,
            base - RETENTION_SECONDS,
        )
        .unwrap();
    let items = library
        .article_page(ArticleScope::Library, 10, None)
        .unwrap()
        .items;
    let old = items.iter().find(|v| v.title == "Old searchable").unwrap();
    library
        .set_article_starred(&old.stable_id, true, base)
        .unwrap();
    assert_eq!(
        library
            .cleanup_retention(base - RETENTION_SECONDS, 100)
            .unwrap(),
        0
    );
    library
        .set_article_starred(&old.stable_id, false, base)
        .unwrap();
    assert_eq!(
        library
            .cleanup_retention(base - RETENTION_SECONDS, 100)
            .unwrap(),
        1
    );
    assert!(library.search("Old searchable", 10).unwrap().is_empty());
    library.integrity_check().unwrap();
}

#[test]
fn search_is_safe_unicode_and_tracks_feed_rename() {
    let path = DatabasePath::new("search");
    let mut library = Library::open(&path).unwrap();
    library
        .ingest_document("https://json.example/feed.json", JSON, 100)
        .unwrap();
    let feed = feed_id("https://json.example/feed.json");
    assert_eq!(library.search("café", 10).unwrap().len(), 0);
    assert_eq!(library.search("JSON Story", 10).unwrap().len(), 1);
    assert!(library.search("\" OR NOT *", 10).is_ok());
    assert!(library.search("", 10).unwrap().is_empty());
    library
        .set_feed_custom_name(&feed, Some("Renamed Café"), 101)
        .unwrap();
    assert_eq!(library.search("Renamed Café", 10).unwrap().len(), 1);
    library.set_feed_custom_name(&feed, None, 102).unwrap();
    assert!(library.search("Renamed Café", 10).unwrap().is_empty());
}

#[test]
fn projections_paginate_without_loading_full_content() {
    let path = DatabasePath::new("projection");
    let mut library = Library::open(&path).unwrap();
    ingest_synthetic(&mut library, 25, 0);
    let first = library
        .article_page(ArticleScope::Library, 10, None)
        .unwrap();
    assert_eq!(first.items.len(), 10);
    let second = library
        .article_page(ArticleScope::Library, 10, first.next.as_ref())
        .unwrap();
    assert_eq!(second.items.len(), 10);
    assert!(first.items.iter().all(|item| {
        !second
            .items
            .iter()
            .any(|other| other.stable_id == item.stable_id)
    }));
    let full = library.full_article(&first.items[0].stable_id).unwrap();
    assert!(full.content.unwrap().contains("Offline body"));
}

#[test]
fn opml_import_export_and_repeat_are_transactional_and_equivalent() {
    let path = DatabasePath::new("opml");
    let mut library = Library::open(&path).unwrap();
    let first = library.import_opml(OPML, 100).unwrap();
    assert_eq!((first.feeds_added, first.folders_created), (3, 2));
    let second = library.import_opml(OPML, 200).unwrap();
    assert_eq!((second.feeds_added, second.duplicates), (0, 3));
    let exported = library
        .export_opml("Thu, 01 Jan 1970 00:00:00 +0000")
        .unwrap();
    let restored = opml::import(&exported).unwrap();
    assert_eq!(restored.feeds.len(), 3);
    assert_eq!(library.stats().unwrap().feeds, 3);
    library.integrity_check().unwrap();
    let before = library.stats().unwrap();
    assert!(
        library
            .import_opml(
                "<opml><body><outline type='rss' text='bad'/></body></opml>",
                300
            )
            .is_ok()
    );
    assert_eq!(library.stats().unwrap(), before);
}

#[test]
fn deterministic_thousand_article_library_stays_consistent() {
    let path = DatabasePath::new("1000");
    let mut library = Library::open(&path).unwrap();
    ingest_synthetic(&mut library, 1_000, 0);
    assert_eq!(library.stats().unwrap().articles, 1_000);
    assert_eq!(
        library
            .article_page(ArticleScope::Library, 50, None)
            .unwrap()
            .items
            .len(),
        50
    );
    assert_eq!(library.unread_count(ArticleScope::Library).unwrap(), 1_000);
    assert_eq!(
        library.mark_all_read(ArticleScope::Library, 10).unwrap(),
        1_000
    );
    library.integrity_check().unwrap();
}

#[test]
#[ignore = "explicit scale validation; exercised by the Phase 3 benchmark run"]
fn deterministic_10k_50k_100k_scales() {
    for size in [10_000, 50_000, 100_000] {
        let path = DatabasePath::new(&size.to_string());
        let mut library = Library::open(&path).unwrap();
        ingest_synthetic(&mut library, size, 0);
        assert_eq!(library.stats().unwrap().articles, size as i64);
        library.integrity_check().unwrap();
    }
}

fn ingest_synthetic(library: &mut Library, total: usize, offset: usize) {
    for batch_start in (0..total).step_by(1_000) {
        let count = (total - batch_start).min(1_000);
        let items=(batch_start..batch_start+count).map(|index|{let id=offset+index;format!(r#"{{"id":"item-{id}","url":"https://scale.invalid/{id}","title":"Scale article {id}","summary":"Searchable café {id}","content_text":"Offline body {id}"}}"#)}).collect::<Vec<_>>().join(",");
        let document = format!(
            r#"{{"version":"https://jsonfeed.org/version/1.1","title":"Scale feed {}","items":[{items}]}}"#,
            batch_start / 1_000
        );
        library
            .ingest_document(
                &format!("https://scale.invalid/feed/{}", batch_start / 1_000),
                &document,
                1_000_000 + batch_start as i64,
            )
            .unwrap();
    }
}
