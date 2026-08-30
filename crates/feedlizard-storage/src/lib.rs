mod error;
mod models;
mod schema;

pub use error::StorageError;
pub use models::*;
pub use schema::SCHEMA_VERSION;

use error::sqlite;
use feedlizard_core::{
    identity::{feed_id, normalize_url},
    ingestion,
    opml::{self, OpmlFeed, OpmlLibrary},
    parser::{self, FeedFormat, ImageSource},
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, params, params_from_iter,
    types::Value,
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

pub struct Library {
    connection: Connection,
}

impl Library {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let mut connection =
            Connection::open(path).map_err(|error| StorageError::Open(error.to_string()))?;
        schema::configure(&connection)?;
        schema::migrate(&mut connection)?;
        Ok(Self { connection })
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| StorageError::Open(error.to_string()))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(2))
            .map_err(sqlite)?;
        Ok(Self { connection })
    }

    pub fn schema_version(&self) -> Result<i64, StorageError> {
        self.connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sqlite)
    }

    pub fn create_folder(
        &mut self,
        name: &str,
        parent_id: Option<i64>,
        now: i64,
    ) -> Result<FolderRecord, StorageError> {
        let name = checked_name(name, "folder name")?;
        let transaction = self.connection.transaction().map_err(sqlite)?;
        let folder = ensure_folder(&transaction, name, parent_id, now)?;
        transaction.commit().map_err(sqlite)?;
        Ok(folder)
    }

    pub fn rename_folder(&mut self, id: i64, name: &str, now: i64) -> Result<(), StorageError> {
        let name = checked_name(name, "folder name")?;
        let changed = self
            .connection
            .execute(
                "UPDATE folders SET name=?1, modified_at=?2 WHERE id=?3",
                params![name, now, id],
            )
            .map_err(sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound("folder"));
        }
        Ok(())
    }

    pub fn delete_folder(&mut self, id: i64) -> Result<(), StorageError> {
        let transaction = self.connection.transaction().map_err(sqlite)?;
        transaction
            .execute("UPDATE folders SET parent_id=NULL WHERE parent_id=?1", [id])
            .map_err(sqlite)?;
        let changed = transaction
            .execute("DELETE FROM folders WHERE id=?1", [id])
            .map_err(sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound("folder"));
        }
        transaction.commit().map_err(sqlite)
    }

    pub fn list_folders(&self) -> Result<Vec<FolderRecord>, StorageError> {
        let mut statement = self.connection.prepare("SELECT id, stable_id, name, parent_id, sort_order FROM folders ORDER BY parent_id, sort_order, name COLLATE NOCASE, id").map_err(sqlite)?;
        statement
            .query_map([], map_folder)
            .map_err(sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite)
    }

    pub fn add_subscription(
        &mut self,
        feed_url: &str,
        title: &str,
        format: FeedFormat,
        site_url: Option<&str>,
        now: i64,
    ) -> Result<String, StorageError> {
        let normalized = normalize_url(feed_url);
        validate_web_url(&normalized)?;
        let stable_id = feed_id(&normalized);
        let title = checked_name(title, "feed title")?;
        self.connection.execute(
            "INSERT INTO feeds(stable_id, normalized_url, fetch_url, site_url, display_name, publisher_name, format, created_at, modified_at) VALUES(?1,?2,?3,?4,?5,?5,?6,?7,?7)",
            params![stable_id, normalized, feed_url, site_url, title, format_string(format), now],
        ).map_err(sqlite)?;
        Ok(stable_id)
    }

    pub fn remove_subscription(&mut self, stable_id: &str) -> Result<(), StorageError> {
        let transaction = self.connection.transaction().map_err(sqlite)?;
        transaction.execute("DELETE FROM article_fts WHERE rowid IN (SELECT rowid FROM articles WHERE feed_stable_id=?1)", [stable_id]).map_err(sqlite)?;
        let changed = transaction
            .execute("DELETE FROM feeds WHERE stable_id=?1", [stable_id])
            .map_err(sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound("feed"));
        }
        transaction.commit().map_err(sqlite)
    }

    pub fn set_feed_custom_name(
        &mut self,
        stable_id: &str,
        custom_name: Option<&str>,
        now: i64,
    ) -> Result<(), StorageError> {
        let value = custom_name
            .map(|name| checked_name(name, "custom feed name"))
            .transpose()?;
        let transaction = self.connection.transaction().map_err(sqlite)?;
        let changed = transaction.execute("UPDATE feeds SET custom_name=?1, display_name=COALESCE(?1,publisher_name), modified_at=?2 WHERE stable_id=?3", params![value, now, stable_id]).map_err(sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound("feed"));
        }
        refresh_feed_fts(&transaction, stable_id)?;
        transaction.commit().map_err(sqlite)
    }

    pub fn move_feed(
        &mut self,
        stable_id: &str,
        folder_id: Option<i64>,
        now: i64,
    ) -> Result<(), StorageError> {
        let changed = self
            .connection
            .execute(
                "UPDATE feeds SET folder_id=?1, modified_at=?2 WHERE stable_id=?3",
                params![folder_id, now, stable_id],
            )
            .map_err(sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound("feed"));
        }
        Ok(())
    }

    pub fn list_feeds(&self) -> Result<Vec<FeedRecord>, StorageError> {
        let mut statement = self.connection.prepare("SELECT stable_id,normalized_url,fetch_url,effective_fetch_url,site_url,display_name,publisher_name,custom_name,format,folder_id,favicon_url,feed_image_url,etag,last_modified,last_refresh_attempt_at,last_refresh_at,last_http_status,consecutive_failures,last_refresh_status FROM feeds ORDER BY display_name COLLATE NOCASE,stable_id").map_err(sqlite)?;
        statement
            .query_map([], map_feed)
            .map_err(sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite)
    }

    pub fn feed(&self, stable_id: &str) -> Result<FeedRecord, StorageError> {
        self.connection
            .query_row("SELECT stable_id,normalized_url,fetch_url,effective_fetch_url,site_url,display_name,publisher_name,custom_name,format,folder_id,favicon_url,feed_image_url,etag,last_modified,last_refresh_attempt_at,last_refresh_at,last_http_status,consecutive_failures,last_refresh_status FROM feeds WHERE stable_id=?1", [stable_id], map_feed)
            .optional()
            .map_err(sqlite)?
            .ok_or(StorageError::NotFound("feed"))
    }

    pub fn set_feed_favicon(
        &mut self,
        stable_id: &str,
        favicon_url: &str,
    ) -> Result<(), StorageError> {
        let changed = self
            .connection
            .execute(
                "UPDATE feeds SET favicon_url=?2, modified_at=strftime('%s','now') WHERE stable_id=?1",
                rusqlite::params![stable_id, favicon_url],
            )
            .map_err(sqlite)?;
        if changed == 0 {
            return Err(StorageError::Constraint("feed does not exist".into()));
        }
        Ok(())
    }

    pub fn record_refresh(
        &mut self,
        stable_id: &str,
        metadata: &RefreshMetadata,
    ) -> Result<(), StorageError> {
        let success = metadata.failure_category.is_none();
        let changed = self.connection.execute(
            "UPDATE feeds SET etag=COALESCE(?1,etag),last_modified=COALESCE(?2,last_modified),last_refresh_attempt_at=?3,last_refresh_at=CASE WHEN ?4 THEN COALESCE(?5,?3) ELSE last_refresh_at END,last_http_status=?6,consecutive_failures=CASE WHEN ?4 THEN 0 ELSE consecutive_failures+1 END,last_refresh_status=CASE WHEN ?4 THEN 'success' ELSE 'failure' END,last_refresh_error=?7,effective_fetch_url=COALESCE(?8,effective_fetch_url),modified_at=?3 WHERE stable_id=?9",
            params![metadata.etag, metadata.last_modified, metadata.attempted_at, success, metadata.succeeded_at, metadata.http_status.map(i64::from), metadata.failure_category, metadata.final_fetch_url, stable_id],
        ).map_err(sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound("feed"));
        }
        Ok(())
    }

    pub fn ingest_document(
        &mut self,
        source_url: &str,
        document: &str,
        now: i64,
    ) -> Result<IngestStats, StorageError> {
        self.ingest_fetched_document(source_url, source_url, document, now, None)
    }

    pub fn ingest_fetched_document(
        &mut self,
        subscription_url: &str,
        content_base_url: &str,
        document: &str,
        now: i64,
        refresh: Option<&RefreshMetadata>,
    ) -> Result<IngestStats, StorageError> {
        let parsed = parser::parse_with_source(document, content_base_url)
            .map_err(|error| StorageError::ImportExport(error.to_string()))?;
        let stable_feed_id = feed_id(subscription_url);
        let existing = self.existing_article_ids(&stable_feed_id)?;
        let prepared = ingestion::prepare(parsed, subscription_url, &existing, now);
        let transaction = self.connection.transaction().map_err(sqlite)?;
        upsert_parsed_feed(&transaction, subscription_url, &prepared, now)?;
        let feed_name: String = transaction
            .query_row(
                "SELECT display_name FROM feeds WHERE stable_id=?1",
                [&prepared.stable_feed_id],
                |row| row.get(0),
            )
            .map_err(sqlite)?;
        let mut stats = IngestStats {
            duplicates_in_document: prepared.duplicates_suppressed,
            ..IngestStats::default()
        };
        for incoming in &prepared.articles {
            let existed: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM articles WHERE stable_id=?1)",
                    [&incoming.stable_id],
                    |row| row.get(0),
                )
                .map_err(sqlite)?;
            upsert_article(&transaction, incoming, &prepared.stable_feed_id, now)?;
            update_fts(
                &transaction,
                &incoming.article.stable_id,
                &incoming.article.title,
                incoming.article.summary.as_deref(),
                incoming.article.content.as_deref(),
                &feed_name,
            )?;
            if existed {
                stats.updated += 1;
            } else {
                stats.inserted += 1;
            }
        }
        if let Some(metadata) = refresh {
            transaction.execute("UPDATE feeds SET etag=COALESCE(?1,etag),last_modified=COALESCE(?2,last_modified),last_refresh_attempt_at=?3,last_refresh_at=COALESCE(?4,?3),last_http_status=?5,consecutive_failures=0,last_refresh_status='success',last_refresh_error=NULL,effective_fetch_url=COALESCE(?6,effective_fetch_url),modified_at=?3 WHERE stable_id=?7", params![metadata.etag,metadata.last_modified,metadata.attempted_at,metadata.succeeded_at,metadata.http_status.map(i64::from),metadata.final_fetch_url,prepared.stable_feed_id]).map_err(sqlite)?;
        } else {
            transaction.execute("UPDATE feeds SET last_refresh_at=?1,last_refresh_status='success',modified_at=?1 WHERE stable_id=?2", params![now, prepared.stable_feed_id]).map_err(sqlite)?;
        }
        transaction.commit().map_err(sqlite)?;
        Ok(stats)
    }

    pub fn mark_article_read(
        &self,
        stable_id: &str,
        read: bool,
        now: i64,
    ) -> Result<(), StorageError> {
        let changed = self
            .connection
            .execute(
                "UPDATE articles SET is_read=?1,modified_at=?2 WHERE stable_id=?3",
                params![read, now, stable_id],
            )
            .map_err(sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound("article"));
        }
        Ok(())
    }

    pub fn set_article_starred(
        &self,
        stable_id: &str,
        starred: bool,
        now: i64,
    ) -> Result<(), StorageError> {
        let changed = self
            .connection
            .execute(
                "UPDATE articles SET is_starred=?1,modified_at=?2 WHERE stable_id=?3",
                params![starred, now, stable_id],
            )
            .map_err(sqlite)?;
        if changed == 0 {
            return Err(StorageError::NotFound("article"));
        }
        Ok(())
    }

    pub fn unstar_all(&mut self, now: i64) -> Result<usize, StorageError> {
        let transaction = self.connection.transaction().map_err(sqlite)?;
        let changed = transaction
            .execute(
                "UPDATE articles SET is_starred=0,modified_at=?1 WHERE is_starred=1",
                [now],
            )
            .map_err(sqlite)?;
        transaction.commit().map_err(sqlite)?;
        Ok(changed)
    }

    pub fn mark_all_read(
        &mut self,
        scope: ArticleScope<'_>,
        now: i64,
    ) -> Result<usize, StorageError> {
        let transaction = self.connection.transaction().map_err(sqlite)?;
        let changed = match scope {
            ArticleScope::Library | ArticleScope::Unread => transaction.execute("UPDATE articles SET is_read=1,modified_at=?1 WHERE is_read=0", [now]),
            ArticleScope::Starred => transaction.execute("UPDATE articles SET is_read=1,modified_at=?1 WHERE is_read=0 AND is_starred=1", [now]),
            ArticleScope::Feed(id) => transaction.execute("UPDATE articles SET is_read=1,modified_at=?1 WHERE is_read=0 AND feed_stable_id=?2", params![now,id]),
            ArticleScope::Folder(id) => transaction.execute("UPDATE articles SET is_read=1,modified_at=?1 WHERE is_read=0 AND feed_stable_id IN (SELECT stable_id FROM feeds WHERE folder_id=?2)", params![now,id]),
        }.map_err(sqlite)?;
        transaction.commit().map_err(sqlite)?;
        Ok(changed)
    }

    pub fn unread_count(&self, scope: ArticleScope<'_>) -> Result<i64, StorageError> {
        let (sql, value): (&str, Option<Value>) = match scope {
            ArticleScope::Library | ArticleScope::Unread => {
                ("SELECT count(*) FROM articles WHERE is_read=0", None)
            }
            ArticleScope::Starred => (
                "SELECT count(*) FROM articles WHERE is_read=0 AND is_starred=1",
                None,
            ),
            ArticleScope::Feed(id) => (
                "SELECT count(*) FROM articles WHERE is_read=0 AND feed_stable_id=?1",
                Some(id.to_owned().into()),
            ),
            ArticleScope::Folder(id) => (
                "SELECT count(*) FROM articles a JOIN feeds f ON f.stable_id=a.feed_stable_id WHERE a.is_read=0 AND f.folder_id=?1",
                Some(id.into()),
            ),
        };
        if let Some(value) = value {
            self.connection
                .query_row(sql, [value], |row| row.get(0))
                .map_err(sqlite)
        } else {
            self.connection
                .query_row(sql, [], |row| row.get(0))
                .map_err(sqlite)
        }
    }

    pub fn unread_folder_summary(
        &self,
        limit: usize,
    ) -> Result<Vec<UnreadSummaryItem>, StorageError> {
        let limit = limit.clamp(1, 10) as i64;
        let mut statement = self
            .connection
            .prepare(
                "SELECT folders.id, folders.name, count(articles.stable_id) AS unread
             FROM folders
             JOIN feeds ON feeds.folder_id=folders.id
             JOIN articles ON articles.feed_stable_id=feeds.stable_id AND articles.is_read=0
             GROUP BY folders.id, folders.name
             HAVING unread > 0
             ORDER BY unread DESC, folders.name COLLATE NOCASE, folders.id
             LIMIT ?1",
            )
            .map_err(sqlite)?;
        statement
            .query_map([limit], |row| {
                Ok(UnreadSummaryItem {
                    folder_id: row.get(0)?,
                    folder_name: row.get(1)?,
                    unread: row.get(2)?,
                })
            })
            .map_err(sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite)
    }

    pub fn cleanup_retention(
        &mut self,
        cutoff: i64,
        batch_size: usize,
    ) -> Result<usize, StorageError> {
        let limit = batch_size.clamp(1, 10_000) as i64;
        let transaction = self.connection.transaction().map_err(sqlite)?;
        let selection = "SELECT rowid FROM articles WHERE is_starred=0 AND retention_at < ?1 ORDER BY retention_at,stable_id LIMIT ?2";
        transaction
            .execute(
                &format!("DELETE FROM article_fts WHERE rowid IN ({selection})"),
                params![cutoff, limit],
            )
            .map_err(sqlite)?;
        let deleted = transaction
            .execute(
                &format!("DELETE FROM articles WHERE rowid IN ({selection})"),
                params![cutoff, limit],
            )
            .map_err(sqlite)?;
        transaction.commit().map_err(sqlite)?;
        Ok(deleted)
    }

    pub fn cleanup_feed_retention(
        &mut self,
        feed_id: &str,
        cutoff: i64,
    ) -> Result<usize, StorageError> {
        let transaction = self.connection.transaction().map_err(sqlite)?;
        let selection = "SELECT rowid FROM articles WHERE feed_stable_id=?1 AND is_starred=0 AND retention_at < ?2";
        transaction
            .execute(
                &format!("DELETE FROM article_fts WHERE rowid IN ({selection})"),
                params![feed_id, cutoff],
            )
            .map_err(sqlite)?;
        let deleted = transaction
            .execute(
                &format!("DELETE FROM articles WHERE rowid IN ({selection})"),
                params![feed_id, cutoff],
            )
            .map_err(sqlite)?;
        transaction.commit().map_err(sqlite)?;
        Ok(deleted)
    }

    pub fn article_page(
        &self,
        scope: ArticleScope<'_>,
        limit: usize,
        cursor: Option<&PageCursor>,
    ) -> Result<ArticlePage, StorageError> {
        let limit = limit.clamp(1, 200);
        let mut values = Vec::<Value>::new();
        let mut where_sql = String::from(" WHERE 1=1");
        match scope {
            ArticleScope::Library => {}
            ArticleScope::Unread => where_sql.push_str(" AND a.is_read=0"),
            ArticleScope::Starred => where_sql.push_str(" AND a.is_starred=1"),
            ArticleScope::Feed(id) => {
                where_sql.push_str(" AND a.feed_stable_id=?");
                values.push(id.to_owned().into());
            }
            ArticleScope::Folder(id) => {
                where_sql.push_str(" AND f.folder_id=?");
                values.push(id.into());
            }
        }
        if let Some(cursor) = cursor {
            where_sql.push_str(" AND (COALESCE(a.published_at,a.updated_at,a.inserted_at) < ? OR (COALESCE(a.published_at,a.updated_at,a.inserted_at)=? AND a.stable_id < ?))");
            values.push(cursor.before_timestamp.into());
            values.push(cursor.before_timestamp.into());
            values.push(cursor.before_id.clone().into());
        }
        values.push(((limit + 1) as i64).into());
        let sql = format!(
            "SELECT a.stable_id,a.feed_stable_id,f.display_name,a.url,a.title,a.summary,a.published_at,a.updated_at,a.image_url,a.is_read,a.is_starred,COALESCE(a.published_at,a.updated_at,a.inserted_at) FROM articles a JOIN feeds f ON f.stable_id=a.feed_stable_id{where_sql} ORDER BY COALESCE(a.published_at,a.updated_at,a.inserted_at) DESC,a.stable_id DESC LIMIT ?"
        );
        let mut statement = self.connection.prepare(&sql).map_err(sqlite)?;
        let mut items = statement
            .query_map(params_from_iter(values), map_projection)
            .map_err(sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite)?;
        let has_more = items.len() > limit;
        if has_more {
            items.pop();
        }
        let next = has_more
            .then(|| {
                items.last().map(|item| PageCursor {
                    before_timestamp: item.sort_timestamp,
                    before_id: item.stable_id.clone(),
                })
            })
            .flatten();
        Ok(ArticlePage { items, next })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<ArticleListItem>, StorageError> {
        let Some(query) = safe_fts_query(query) else {
            return Ok(Vec::new());
        };
        let mut statement = self.connection.prepare("SELECT a.stable_id,a.feed_stable_id,f.display_name,a.url,a.title,a.summary,a.published_at,a.updated_at,a.image_url,a.is_read,a.is_starred,COALESCE(a.published_at,a.updated_at,a.inserted_at) FROM article_fts x JOIN articles a ON a.stable_id=x.article_id JOIN feeds f ON f.stable_id=a.feed_stable_id WHERE article_fts MATCH ?1 ORDER BY bm25(article_fts),(a.published_at IS NULL AND a.updated_at IS NULL),COALESCE(a.published_at,a.updated_at,a.inserted_at) DESC,a.stable_id DESC LIMIT ?2").map_err(|error| StorageError::Search(error.to_string()))?;
        statement
            .query_map(params![query, limit.clamp(1, 200) as i64], map_projection)
            .map_err(|error| StorageError::Search(error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StorageError::Search(error.to_string()))
    }

    pub fn full_article(&self, stable_id: &str) -> Result<FullArticle, StorageError> {
        self.connection.query_row("SELECT a.stable_id,a.feed_stable_id,f.display_name,a.provider_id,a.url,a.title,a.author,a.summary,a.content,a.published_at,a.updated_at,a.image_url,a.image_source,a.enclosure_url,a.enclosure_type,a.is_read,a.is_starred,a.inserted_at FROM articles a JOIN feeds f ON f.stable_id=a.feed_stable_id WHERE a.stable_id=?1", [stable_id], map_full_article).optional().map_err(sqlite)?.ok_or(StorageError::NotFound("article"))
    }

    pub fn set_article_image(&self, stable_id: &str, image_url: &str) -> Result<(), StorageError> {
        self.connection
            .execute(
                "UPDATE articles SET image_url=?1,image_source='open-graph' WHERE stable_id=?2 AND image_url IS NULL",
                params![image_url, stable_id],
            )
            .map_err(sqlite)?;
        Ok(())
    }

    pub fn import_opml(&mut self, input: &str, now: i64) -> Result<ImportStats, StorageError> {
        let library =
            opml::import(input).map_err(|error| StorageError::ImportExport(error.to_string()))?;
        let transaction = self.connection.transaction().map_err(sqlite)?;
        let mut stats = ImportStats {
            failed_entries: library.failures.len(),
            ..ImportStats::default()
        };
        for feed in library.feeds {
            let mut parent = None;
            for name in &feed.folders {
                let before: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM folders WHERE parent_id IS ?1 AND name=?2 COLLATE NOCASE)", params![parent,name], |row| row.get(0)).map_err(sqlite)?;
                let folder = ensure_folder(&transaction, name, parent, now)?;
                if !before {
                    stats.folders_created += 1;
                }
                parent = Some(folder.id);
            }
            let stable = feed_id(&feed.feed_url);
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM feeds WHERE stable_id=?1)",
                    [&stable],
                    |row| row.get(0),
                )
                .map_err(sqlite)?;
            if exists {
                stats.duplicates += 1;
                transaction.execute("UPDATE feeds SET folder_id=COALESCE(?1,folder_id),site_url=COALESCE(?2,site_url),fetch_url=?3,modified_at=?4 WHERE stable_id=?5",params![parent,feed.site_url,feed.feed_url,now,stable]).map_err(sqlite)?;
                continue;
            }
            let display = feed.custom_title.as_deref().unwrap_or(&feed.title);
            transaction.execute("INSERT INTO feeds(stable_id,normalized_url,fetch_url,site_url,display_name,publisher_name,custom_name,format,folder_id,created_at,modified_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",params![stable,normalize_url(&feed.feed_url),feed.feed_url,feed.site_url,display,feed.title,feed.custom_title,format_string(feed.format),parent,now]).map_err(sqlite)?;
            stats.feeds_added += 1;
        }
        transaction.commit().map_err(sqlite)?;
        Ok(stats)
    }

    pub fn export_opml(&self, created_rfc2822: &str) -> Result<String, StorageError> {
        let folders = self.list_folders()?;
        let by_id: HashMap<i64, FolderRecord> = folders
            .into_iter()
            .map(|folder| (folder.id, folder))
            .collect();
        let mut library = OpmlLibrary::default();
        for feed in self.list_feeds()? {
            let mut path = Vec::new();
            let mut parent = feed.folder_id;
            let mut guard = 0;
            while let Some(id) = parent {
                guard += 1;
                if guard > 32 {
                    return Err(StorageError::Corruption("folder cycle".into()));
                }
                let folder = by_id
                    .get(&id)
                    .ok_or_else(|| StorageError::Corruption("orphan folder".into()))?;
                path.push(folder.name.clone());
                parent = folder.parent_id;
            }
            path.reverse();
            library.feeds.push(OpmlFeed {
                title: feed.display_name,
                feed_url: feed.fetch_url,
                site_url: feed.site_url,
                folders: path,
                format: feed.format,
                custom_title: feed.custom_name,
            });
        }
        Ok(opml::export(&library, created_rfc2822))
    }

    pub fn stats(&self) -> Result<LibraryStats, StorageError> {
        Ok(LibraryStats {
            feeds: count(&self.connection, "feeds")?,
            folders: count(&self.connection, "folders")?,
            articles: count(&self.connection, "articles")?,
            unread: self
                .connection
                .query_row("SELECT count(*) FROM articles WHERE is_read=0", [], |r| {
                    r.get(0)
                })
                .map_err(sqlite)?,
            starred: self
                .connection
                .query_row(
                    "SELECT count(*) FROM articles WHERE is_starred=1",
                    [],
                    |r| r.get(0),
                )
                .map_err(sqlite)?,
        })
    }

    pub fn integrity_check(&self) -> Result<(), StorageError> {
        let result: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .map_err(sqlite)?;
        if result != "ok" {
            return Err(StorageError::Corruption(result));
        }
        let foreign: i64 = self
            .connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .map_err(sqlite)?;
        let missing:i64=self.connection.query_row("SELECT count(*) FROM articles a LEFT JOIN article_fts x ON x.rowid=a.rowid WHERE x.rowid IS NULL",[],|r|r.get(0)).map_err(sqlite)?;
        let excess:i64=self.connection.query_row("SELECT count(*) FROM article_fts x LEFT JOIN articles a ON a.rowid=x.rowid WHERE a.rowid IS NULL",[],|r|r.get(0)).map_err(sqlite)?;
        if foreign + missing + excess != 0 {
            return Err(StorageError::Corruption(format!(
                "foreign={foreign} fts_missing={missing} fts_excess={excess}"
            )));
        }
        Ok(())
    }

    fn existing_article_ids(&self, feed_id: &str) -> Result<HashSet<String>, StorageError> {
        let mut statement = self
            .connection
            .prepare("SELECT stable_id FROM articles WHERE feed_stable_id=?1")
            .map_err(sqlite)?;
        statement
            .query_map([feed_id], |r| r.get(0))
            .map_err(sqlite)?
            .collect::<Result<_, _>>()
            .map_err(sqlite)
    }
}

fn upsert_parsed_feed(
    transaction: &Transaction<'_>,
    source_url: &str,
    prepared: &ingestion::IngestionResult,
    now: i64,
) -> Result<(), StorageError> {
    let feed = &prepared.feed;
    let publisher = checked_name(&feed.title, "feed title")?;
    let normalized = normalize_url(source_url);
    validate_web_url(&normalized)?;
    let icons = (!feed.icon_candidates.is_empty()).then(|| feed.icon_candidates.join("\n"));
    transaction.execute("INSERT INTO feeds(stable_id,normalized_url,fetch_url,site_url,display_name,publisher_name,description,format,favicon_url,icon_candidates,feed_image_url,created_at,modified_at) VALUES(?1,?2,?3,?4,?5,?5,?6,?7,?8,?9,?10,?11,?11) ON CONFLICT(stable_id) DO UPDATE SET normalized_url=excluded.normalized_url,site_url=COALESCE(excluded.site_url,feeds.site_url),publisher_name=excluded.publisher_name,display_name=COALESCE(feeds.custom_name,excluded.publisher_name),description=COALESCE(excluded.description,feeds.description),format=excluded.format,favicon_url=COALESCE(excluded.favicon_url,feeds.favicon_url),icon_candidates=COALESCE(excluded.icon_candidates,feeds.icon_candidates),feed_image_url=COALESCE(excluded.feed_image_url,feeds.feed_image_url),modified_at=excluded.modified_at",params![prepared.stable_feed_id,normalized,source_url,feed.site_url,publisher,feed.description,format_string(feed.format),feed.icon_candidates.first(),icons,feed.feed_image,now]).map_err(sqlite)?;
    refresh_feed_fts(transaction, &prepared.stable_feed_id)
}

fn upsert_article(
    transaction: &Transaction<'_>,
    incoming: &ingestion::IngestArticle,
    feed_id: &str,
    now: i64,
) -> Result<(), StorageError> {
    let article = &incoming.article;
    let (image_url, image_source) = article
        .image
        .as_ref()
        .map(|image| {
            (
                Some(image.url.as_str()),
                Some(image_source_string(image.source)),
            )
        })
        .unwrap_or((None, None));
    transaction.execute("INSERT INTO articles(stable_id,feed_stable_id,provider_id,url,title,author,summary,content,published_at,updated_at,inserted_at,image_url,image_source,enclosure_url,enclosure_type,retention_at,modified_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?11) ON CONFLICT(stable_id) DO UPDATE SET url=COALESCE(excluded.url,articles.url),title=excluded.title,author=COALESCE(excluded.author,articles.author),summary=COALESCE(excluded.summary,articles.summary),content=COALESCE(excluded.content,articles.content),published_at=COALESCE(excluded.published_at,articles.published_at),updated_at=COALESCE(excluded.updated_at,articles.updated_at),image_url=COALESCE(excluded.image_url,articles.image_url),image_source=COALESCE(excluded.image_source,articles.image_source),enclosure_url=COALESCE(excluded.enclosure_url,articles.enclosure_url),enclosure_type=COALESCE(excluded.enclosure_type,articles.enclosure_type),retention_at=COALESCE(excluded.published_at,articles.retention_at),modified_at=excluded.modified_at",params![incoming.stable_id,feed_id,incoming.provider_id,article.url,article.title,article.author,article.summary,article.content,article.published_at,article.updated_at,now,image_url,image_source,article.enclosure_url,article.enclosure_type,incoming.retention_timestamp]).map_err(sqlite)?;
    Ok(())
}

fn update_fts(
    transaction: &Transaction<'_>,
    id: &str,
    title: &str,
    summary: Option<&str>,
    content: Option<&str>,
    feed_name: &str,
) -> Result<(), StorageError> {
    let rowid: i64 = transaction
        .query_row(
            "SELECT rowid FROM articles WHERE stable_id=?1",
            [id],
            |row| row.get(0),
        )
        .map_err(|error| StorageError::Search(error.to_string()))?;
    transaction
        .execute("DELETE FROM article_fts WHERE rowid=?1", [rowid])
        .map_err(|e| StorageError::Search(e.to_string()))?;
    transaction.execute("INSERT INTO article_fts(rowid,article_id,title,summary,content,feed_name) VALUES(?1,?2,?3,?4,?5,?6)",params![rowid,id,title,summary,content,feed_name]).map_err(|e|StorageError::Search(e.to_string()))?;
    Ok(())
}
fn refresh_feed_fts(transaction: &Transaction<'_>, feed_id: &str) -> Result<(), StorageError> {
    transaction.execute("UPDATE article_fts SET feed_name=(SELECT display_name FROM feeds WHERE stable_id=?1) WHERE article_id IN (SELECT stable_id FROM articles WHERE feed_stable_id=?1)",[feed_id]).map_err(|e|StorageError::Search(e.to_string()))?;
    Ok(())
}

fn ensure_folder(
    transaction: &Transaction<'_>,
    name: &str,
    parent_id: Option<i64>,
    now: i64,
) -> Result<FolderRecord, StorageError> {
    if let Some(folder)=transaction.query_row("SELECT id,stable_id,name,parent_id,sort_order FROM folders WHERE parent_id IS ?1 AND name=?2 COLLATE NOCASE",params![parent_id,name],map_folder).optional().map_err(sqlite)? { return Ok(folder); }
    let id: i64 = transaction
        .query_row("SELECT COALESCE(max(id),0)+1 FROM folders", [], |r| {
            r.get(0)
        })
        .map_err(sqlite)?;
    let stable = format!("folder:v1:{id:016x}");
    transaction.execute("INSERT INTO folders(id,stable_id,name,parent_id,created_at,modified_at) VALUES(?1,?2,?3,?4,?5,?5)",params![id,stable,name,parent_id,now]).map_err(sqlite)?;
    Ok(FolderRecord {
        id,
        stable_id: stable,
        name: name.into(),
        parent_id,
        sort_order: 0,
    })
}
fn checked_name<'a>(value: &'a str, kind: &'static str) -> Result<&'a str, StorageError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 1000 {
        Err(StorageError::InvalidInput(kind))
    } else {
        Ok(value)
    }
}
fn validate_web_url(value: &str) -> Result<(), StorageError> {
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(())
    } else {
        Err(StorageError::InvalidInput("feed URL"))
    }
}
fn safe_fts_query(value: &str) -> Option<String> {
    let terms: Vec<_> = value
        .split_whitespace()
        .filter(|v| !v.is_empty())
        .take(20)
        .map(|v| format!("\"{}\"*", v.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}
fn format_string(value: FeedFormat) -> &'static str {
    match value {
        FeedFormat::Rss => "rss",
        FeedFormat::Atom => "atom",
        FeedFormat::Json => "json",
    }
}
fn parse_format(value: String) -> Result<FeedFormat, rusqlite::Error> {
    match value.as_str() {
        "rss" => Ok(FeedFormat::Rss),
        "atom" => Ok(FeedFormat::Atom),
        "json" => Ok(FeedFormat::Json),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
fn image_source_string(value: ImageSource) -> &'static str {
    match value {
        ImageSource::InlineHtml => "inline",
        ImageSource::MediaThumbnail => "media-thumbnail",
        ImageSource::MediaContent => "media-content",
        ImageSource::Enclosure => "enclosure",
        ImageSource::JsonImage => "json-image",
    }
}
fn map_folder(row: &Row<'_>) -> rusqlite::Result<FolderRecord> {
    Ok(FolderRecord {
        id: row.get(0)?,
        stable_id: row.get(1)?,
        name: row.get(2)?,
        parent_id: row.get(3)?,
        sort_order: row.get(4)?,
    })
}
fn map_feed(row: &Row<'_>) -> rusqlite::Result<FeedRecord> {
    Ok(FeedRecord {
        stable_id: row.get(0)?,
        normalized_url: row.get(1)?,
        fetch_url: row.get(2)?,
        effective_fetch_url: row.get(3)?,
        site_url: row.get(4)?,
        display_name: row.get(5)?,
        publisher_name: row.get(6)?,
        custom_name: row.get(7)?,
        format: parse_format(row.get(8)?)?,
        folder_id: row.get(9)?,
        favicon_url: row.get(10)?,
        feed_image_url: row.get(11)?,
        etag: row.get(12)?,
        last_modified: row.get(13)?,
        last_refresh_attempt_at: row.get(14)?,
        last_refresh_at: row.get(15)?,
        last_http_status: row
            .get::<_, Option<i64>>(16)?
            .and_then(|value| u16::try_from(value).ok()),
        consecutive_failures: row.get::<_, i64>(17)?.try_into().unwrap_or(u32::MAX),
        last_refresh_status: row.get(18)?,
    })
}
fn map_projection(row: &Row<'_>) -> rusqlite::Result<ArticleListItem> {
    Ok(ArticleListItem {
        stable_id: row.get(0)?,
        feed_stable_id: row.get(1)?,
        feed_name: row.get(2)?,
        article_url: row.get(3)?,
        title: row.get(4)?,
        summary: row.get(5)?,
        published_at: row.get(6)?,
        updated_at: row.get(7)?,
        thumbnail_url: row.get(8)?,
        is_unread: !row.get::<_, bool>(9)?,
        is_starred: row.get(10)?,
        sort_timestamp: row.get(11)?,
    })
}
fn map_full_article(row: &Row<'_>) -> rusqlite::Result<FullArticle> {
    Ok(FullArticle {
        stable_id: row.get(0)?,
        feed_stable_id: row.get(1)?,
        feed_name: row.get(2)?,
        provider_id: row.get(3)?,
        url: row.get(4)?,
        title: row.get(5)?,
        author: row.get(6)?,
        summary: row.get(7)?,
        content: row.get(8)?,
        published_at: row.get(9)?,
        inserted_at: row.get(17)?,
        updated_at: row.get(10)?,
        image_url: row.get(11)?,
        image_source: row.get(12)?,
        enclosure_url: row.get(13)?,
        enclosure_type: row.get(14)?,
        is_read: row.get(15)?,
        is_starred: row.get(16)?,
    })
}
fn count(connection: &Connection, table: &str) -> Result<i64, StorageError> {
    let sql = format!("SELECT count(*) FROM {table}");
    connection.query_row(&sql, [], |r| r.get(0)).map_err(sqlite)
}
