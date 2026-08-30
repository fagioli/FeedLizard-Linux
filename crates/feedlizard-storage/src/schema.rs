use crate::error::StorageError;
use rusqlite::Connection;
use std::time::Duration;

pub const SCHEMA_VERSION: i64 = 3;

const MIGRATION_1: &str = r#"
CREATE TABLE folders (
    id INTEGER PRIMARY KEY,
    stable_id TEXT NOT NULL UNIQUE CHECK(length(stable_id) BETWEEN 1 AND 200),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 500),
    parent_id INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    modified_at INTEGER NOT NULL,
    UNIQUE(parent_id, name COLLATE NOCASE)
);

CREATE TABLE feeds (
    stable_id TEXT PRIMARY KEY CHECK(length(stable_id) BETWEEN 1 AND 200),
    normalized_url TEXT NOT NULL UNIQUE CHECK(length(normalized_url) BETWEEN 1 AND 4096),
    fetch_url TEXT NOT NULL CHECK(length(fetch_url) BETWEEN 1 AND 4096),
    site_url TEXT,
    display_name TEXT NOT NULL CHECK(length(display_name) BETWEEN 1 AND 1000),
    publisher_name TEXT NOT NULL CHECK(length(publisher_name) BETWEEN 1 AND 1000),
    custom_name TEXT,
    description TEXT,
    format TEXT NOT NULL CHECK(format IN ('rss', 'atom', 'json')),
    folder_id INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    favicon_url TEXT,
    icon_candidates TEXT,
    feed_image_url TEXT,
    etag TEXT,
    last_modified TEXT,
    last_refresh_at INTEGER,
    last_refresh_status TEXT,
    created_at INTEGER NOT NULL,
    modified_at INTEGER NOT NULL
);

CREATE TABLE articles (
    stable_id TEXT PRIMARY KEY CHECK(length(stable_id) BETWEEN 1 AND 200),
    feed_stable_id TEXT NOT NULL REFERENCES feeds(stable_id) ON DELETE CASCADE,
    provider_id TEXT,
    url TEXT,
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 4000),
    author TEXT,
    summary TEXT,
    content TEXT,
    published_at INTEGER,
    updated_at INTEGER,
    inserted_at INTEGER NOT NULL,
    image_url TEXT,
    image_source TEXT,
    enclosure_url TEXT,
    enclosure_type TEXT,
    is_read INTEGER NOT NULL DEFAULT 0 CHECK(is_read IN (0, 1)),
    is_starred INTEGER NOT NULL DEFAULT 0 CHECK(is_starred IN (0, 1)),
    retention_at INTEGER NOT NULL,
    modified_at INTEGER NOT NULL
);

CREATE INDEX feeds_folder_idx ON feeds(folder_id, display_name, stable_id);
CREATE INDEX articles_feed_order_idx ON articles(feed_stable_id, published_at DESC, inserted_at DESC, stable_id DESC);
CREATE INDEX articles_library_order_idx ON articles(COALESCE(published_at, inserted_at) DESC, stable_id DESC);
CREATE INDEX articles_unread_order_idx ON articles(is_read, COALESCE(published_at, inserted_at) DESC, stable_id DESC);
CREATE INDEX articles_starred_order_idx ON articles(is_starred, COALESCE(published_at, inserted_at) DESC, stable_id DESC);
CREATE INDEX articles_retention_idx ON articles(is_starred, retention_at, stable_id);

CREATE VIRTUAL TABLE article_fts USING fts5(
    article_id UNINDEXED,
    title,
    summary,
    content,
    feed_name,
    tokenize = 'unicode61 remove_diacritics 2'
);
"#;

const MIGRATION_2: &str = r#"
ALTER TABLE feeds ADD COLUMN last_refresh_attempt_at INTEGER;
ALTER TABLE feeds ADD COLUMN last_http_status INTEGER;
ALTER TABLE feeds ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0;
ALTER TABLE feeds ADD COLUMN last_refresh_error TEXT;
ALTER TABLE feeds ADD COLUMN effective_fetch_url TEXT;
"#;

// Earlier beta builds could persist articles before all publisher date formats
// were understood. Conditional requests then kept those rows undated forever.
// Clear validators once for affected feeds so their next ordinary refresh can
// safely reparse the current document. No subscriptions or article state move.
const MIGRATION_3: &str = r#"
UPDATE feeds
SET etag = NULL, last_modified = NULL
WHERE EXISTS (
    SELECT 1 FROM articles
    WHERE articles.feed_stable_id = feeds.stable_id
      AND articles.published_at IS NULL
      AND articles.updated_at IS NULL
);

DROP INDEX articles_feed_order_idx;
DROP INDEX articles_library_order_idx;
DROP INDEX articles_unread_order_idx;
DROP INDEX articles_starred_order_idx;
CREATE INDEX articles_feed_order_idx ON articles(feed_stable_id, COALESCE(published_at, updated_at, inserted_at) DESC, stable_id DESC);
CREATE INDEX articles_library_order_idx ON articles(COALESCE(published_at, updated_at, inserted_at) DESC, stable_id DESC);
CREATE INDEX articles_unread_order_idx ON articles(is_read, COALESCE(published_at, updated_at, inserted_at) DESC, stable_id DESC);
CREATE INDEX articles_starred_order_idx ON articles(is_starred, COALESCE(published_at, updated_at, inserted_at) DESC, stable_id DESC);
"#;

pub(crate) fn configure(connection: &Connection) -> Result<(), StorageError> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| StorageError::Open(error.to_string()))?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| StorageError::Open(error.to_string()))?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| StorageError::Open(error.to_string()))?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(|error| StorageError::Open(error.to_string()))?;
    connection
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(|error| StorageError::Open(error.to_string()))?;
    Ok(())
}

pub(crate) fn migrate(connection: &mut Connection) -> Result<(), StorageError> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    if version > SCHEMA_VERSION {
        return Err(StorageError::UnsupportedSchema(version));
    }
    if version == 0 {
        apply_migration(connection, 1, MIGRATION_1)?;
    }
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    if version == 1 {
        apply_migration(connection, 2, MIGRATION_2)?;
    }
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    if version == 2 {
        apply_migration(connection, 3, MIGRATION_3)?;
    }
    Ok(())
}

fn apply_migration(
    connection: &mut Connection,
    version: i64,
    sql: &str,
) -> Result<(), StorageError> {
    let transaction = connection
        .transaction()
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    transaction
        .execute_batch(sql)
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    transaction
        .pragma_update(None, "user_version", version)
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    transaction
        .commit()
        .map_err(|error| StorageError::Migration(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_migration_rolls_back_ddl_and_version() {
        let mut connection = Connection::open_in_memory().unwrap();
        let result = apply_migration(&mut connection, 1, "CREATE TABLE partial(id); INVALID SQL;");
        assert!(result.is_err());
        let exists: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='partial'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!((exists, version), (0, 0));
    }

    #[test]
    fn version_one_upgrades_to_current_schema_transactionally() {
        let mut connection = Connection::open_in_memory().unwrap();
        configure(&connection).unwrap();
        apply_migration(&mut connection, 1, MIGRATION_1).unwrap();
        migrate(&mut connection).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('feeds') WHERE name IN ('last_refresh_attempt_at','last_http_status','consecutive_failures','last_refresh_error','effective_fetch_url')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((version, columns), (3, 5));
    }

    #[test]
    fn version_three_refetches_only_feeds_with_undated_articles() {
        let mut connection = Connection::open_in_memory().unwrap();
        configure(&connection).unwrap();
        apply_migration(&mut connection, 1, MIGRATION_1).unwrap();
        apply_migration(&mut connection, 2, MIGRATION_2).unwrap();
        connection.execute_batch(
            r#"
            INSERT INTO feeds(stable_id,normalized_url,fetch_url,display_name,publisher_name,format,etag,last_modified,created_at,modified_at)
            VALUES ('undated','https://undated.example/feed','https://undated.example/feed','Undated','Undated','rss','etag-a','date-a',1,1),
                   ('dated','https://dated.example/feed','https://dated.example/feed','Dated','Dated','rss','etag-b','date-b',1,1);
            INSERT INTO articles(stable_id,feed_stable_id,title,published_at,inserted_at,is_read,is_starred,retention_at,modified_at)
            VALUES ('article-a','undated','Undated article',NULL,10,0,0,10,10),
                   ('article-b','dated','Dated article',9,10,0,0,10,10);
            "#,
        ).unwrap();

        migrate(&mut connection).unwrap();

        let undated: (Option<String>, Option<String>) = connection
            .query_row(
                "SELECT etag,last_modified FROM feeds WHERE stable_id='undated'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let dated: (Option<String>, Option<String>) = connection
            .query_row(
                "SELECT etag,last_modified FROM feeds WHERE stable_id='dated'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(undated, (None, None));
        assert_eq!(dated, (Some("etag-b".into()), Some("date-b".into())));
    }
}
