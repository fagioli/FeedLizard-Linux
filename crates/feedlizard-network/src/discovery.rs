use feedlizard_core::discovery::{DiscoveryLink, rank_candidates};
use scraper::{Html, Selector};

use crate::{NetworkError, NetworkErrorKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFeed {
    pub url: String,
    pub format_hint: String,
    pub title_hint: Option<String>,
    pub source_page: String,
    pub rank: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryResult {
    pub source_page: String,
    pub candidates: Vec<DiscoveredFeed>,
}

pub(crate) fn discover(source_page: &str, html: &str) -> Result<DiscoveryResult, NetworkError> {
    let selector = Selector::parse("link[rel~='alternate'][href][type]").map_err(|_| {
        NetworkError::new(
            NetworkErrorKind::InvalidResponse,
            "invalid discovery selector",
        )
    })?;
    let document = Html::parse_document(html);
    let links = document
        .select(&selector)
        .map(|element| DiscoveryLink {
            href: element.value().attr("href").unwrap_or_default().to_owned(),
            mime_type: element.value().attr("type").unwrap_or_default().to_owned(),
            title: element.value().attr("title").map(ToOwned::to_owned),
        })
        .collect::<Vec<_>>();
    let candidates = rank_candidates(source_page, &links)
        .into_iter()
        .enumerate()
        .map(|(rank, candidate)| DiscoveredFeed {
            url: candidate.url,
            format_hint: format!("{:?}", candidate.format).to_ascii_lowercase(),
            title_hint: candidate.title,
            source_page: source_page.to_owned(),
            rank,
        })
        .collect();
    Ok(DiscoveryResult {
        source_page: source_page.to_owned(),
        candidates,
    })
}
