use std::collections::HashSet;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiscoveryFormat {
    Rss,
    Atom,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryLink {
    pub href: String,
    pub mime_type: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedCandidate {
    pub url: String,
    pub format: DiscoveryFormat,
    pub title: Option<String>,
}

pub fn rank_candidates(base_url: &str, links: &[DiscoveryLink]) -> Vec<FeedCandidate> {
    let Ok(base) = Url::parse(base_url) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for link in links {
        let mime = link
            .mime_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let format = match mime.as_str() {
            "application/rss+xml" | "application/rdf+xml" => DiscoveryFormat::Rss,
            "application/atom+xml" => DiscoveryFormat::Atom,
            "application/feed+json" | "application/json" => DiscoveryFormat::Json,
            _ => continue,
        };
        let Ok(url) = base.join(link.href.trim()) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") {
            continue;
        }
        let key = url.as_str().to_owned();
        if seen.insert(key.clone()) {
            result.push(FeedCandidate {
                url: key,
                format,
                title: link.title.clone(),
            });
        }
    }
    result.sort_by_key(|candidate| candidate.format);
    result
}
