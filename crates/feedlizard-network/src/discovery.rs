use feedlizard_core::discovery::{DiscoveryLink, rank_candidates};
use scraper::{Html, Selector};

use crate::{NetworkError, NetworkErrorKind};
use url::Url;

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

pub(crate) fn article_image(source_page: &str, html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    for selector_text in [
        "meta[property='og:image'][content]",
        "meta[property='og:image:secure_url'][content]",
        "meta[name='twitter:image'][content]",
        "meta[name='twitter:image:src'][content]",
        "link[rel='image_src'][href]",
    ] {
        let selector = Selector::parse(selector_text).ok()?;
        for element in document.select(&selector) {
            let candidate = element
                .value()
                .attr("content")
                .or_else(|| element.value().attr("href"))?;
            let url = Url::parse(source_page).ok()?.join(candidate).ok()?;
            if matches!(url.scheme(), "http" | "https") {
                return Some(url.to_string());
            }
        }
    }
    None
}

pub(crate) fn site_icon(source_page: &str, html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("link[href][rel]").ok()?;
    let base = Url::parse(source_page).ok()?;
    let mut candidates = document
        .select(&selector)
        .filter_map(|element| {
            let rel = element.value().attr("rel")?.to_ascii_lowercase();
            if !rel.split_whitespace().any(|value| {
                matches!(
                    value,
                    "icon" | "shortcut" | "apple-touch-icon" | "mask-icon"
                )
            }) {
                return None;
            }
            let url = base.join(element.value().attr("href")?).ok()?;
            if !matches!(url.scheme(), "http" | "https") {
                return None;
            }
            let mime = element.value().attr("type").unwrap_or_default();
            let is_svg = mime.eq_ignore_ascii_case("image/svg+xml")
                || url.path().to_ascii_lowercase().ends_with(".svg");
            let size = element
                .value()
                .attr("sizes")
                .and_then(|value| value.split('x').next())
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0);
            let rank = (is_svg, std::cmp::Reverse(size), rel.contains("mask-icon"));
            Some((rank, url.to_string()))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(rank, _)| *rank);
    candidates.into_iter().map(|(_, url)| url).next()
}

#[cfg(test)]
mod tests {
    use super::{article_image, site_icon};

    #[test]
    fn discovers_safe_open_graph_image_and_resolves_relative_urls() {
        assert_eq!(
            article_image(
                "https://example.com/news/story",
                r#"<meta property="og:image" content="/images/hero.jpg">"#,
            ),
            Some("https://example.com/images/hero.jpg".into())
        );
        assert_eq!(
            article_image(
                "https://example.com/news/story",
                r#"<meta property="og:image" content="javascript:alert(1)">"#,
            ),
            None
        );
    }

    #[test]
    fn prefers_large_raster_site_icons_and_resolves_relative_urls() {
        let html = r#"
            <link rel="icon" type="image/svg+xml" href="/icon.svg">
            <link rel="icon" type="image/png" sizes="32x32" href="/small.png">
            <link rel="apple-touch-icon" sizes="180x180" href="assets/touch.png">
        "#;
        assert_eq!(
            site_icon("https://example.com/news/", html),
            Some("https://example.com/news/assets/touch.png".into())
        );
    }
}
