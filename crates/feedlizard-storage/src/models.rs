use feedlizard_core::parser::FeedFormat;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRecord {
    pub id: i64,
    pub stable_id: String,
    pub name: String,
    pub parent_id: Option<i64>,
    pub sort_order: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedRecord {
    pub stable_id: String,
    pub normalized_url: String,
    pub fetch_url: String,
    pub effective_fetch_url: Option<String>,
    pub site_url: Option<String>,
    pub display_name: String,
    pub publisher_name: String,
    pub custom_name: Option<String>,
    pub format: FeedFormat,
    pub folder_id: Option<i64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub last_refresh_attempt_at: Option<i64>,
    pub last_refresh_at: Option<i64>,
    pub last_http_status: Option<u16>,
    pub consecutive_failures: u32,
    pub last_refresh_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshMetadata {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub attempted_at: i64,
    pub succeeded_at: Option<i64>,
    pub http_status: Option<u16>,
    pub failure_category: Option<String>,
    pub final_fetch_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleListItem {
    pub stable_id: String,
    pub feed_stable_id: String,
    pub feed_name: String,
    pub title: String,
    pub summary: Option<String>,
    pub published_at: Option<i64>,
    pub thumbnail_url: Option<String>,
    pub is_unread: bool,
    pub is_starred: bool,
    pub sort_timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullArticle {
    pub stable_id: String,
    pub feed_stable_id: String,
    pub feed_name: String,
    pub provider_id: Option<String>,
    pub url: Option<String>,
    pub title: String,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub published_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub image_url: Option<String>,
    pub image_source: Option<String>,
    pub enclosure_url: Option<String>,
    pub enclosure_type: Option<String>,
    pub is_read: bool,
    pub is_starred: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArticleScope<'a> {
    Library,
    Unread,
    Starred,
    Feed(&'a str),
    Folder(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCursor {
    pub before_timestamp: i64,
    pub before_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticlePage {
    pub items: Vec<ArticleListItem>,
    pub next: Option<PageCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IngestStats {
    pub inserted: usize,
    pub updated: usize,
    pub duplicates_in_document: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportStats {
    pub feeds_added: usize,
    pub duplicates: usize,
    pub folders_created: usize,
    pub failed_entries: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LibraryStats {
    pub feeds: i64,
    pub folders: i64,
    pub articles: i64,
    pub unread: i64,
    pub starred: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadSummaryItem {
    pub folder_id: i64,
    pub folder_name: String,
    pub unread: i64,
}
