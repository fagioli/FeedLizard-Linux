use crate::{
    identity::{article_id, feed_id},
    parser::{ParsedArticle, ParsedFeed},
};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestArticle {
    pub stable_id: String,
    pub provider_id: String,
    pub article: ParsedArticle,
    pub is_existing: bool,
    pub retention_timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestionResult {
    pub stable_feed_id: String,
    pub feed: ParsedFeed,
    pub articles: Vec<IngestArticle>,
    pub duplicates_suppressed: usize,
}

pub fn prepare(
    mut feed: ParsedFeed,
    subscription_url: &str,
    existing: &HashSet<String>,
    inserted_at: i64,
) -> IngestionResult {
    let stable_feed_id = feed_id(subscription_url);
    let mut seen = HashSet::new();
    let mut duplicates = 0;
    let mut articles = Vec::new();
    for mut article in feed.articles.drain(..) {
        let provider_id = article.stable_id.clone();
        let id = article_id(
            &stable_feed_id,
            Some(&provider_id),
            article.url.as_deref(),
            Some(&article.title),
            article.published_at,
        );
        article.stable_id = id.clone();
        if !seen.insert(id.clone()) {
            duplicates += 1;
            continue;
        }
        articles.push(IngestArticle {
            stable_id: id.clone(),
            provider_id,
            retention_timestamp: article.published_at.unwrap_or(inserted_at),
            is_existing: existing.contains(&id),
            article,
        });
    }
    feed.articles = articles.iter().map(|v| v.article.clone()).collect();
    IngestionResult {
        stable_feed_id,
        feed,
        articles,
        duplicates_suppressed: duplicates,
    }
}
