use crate::error::CoreError;
use chrono::{DateTime, NaiveDate, Utc};
use roxmltree::{Document, Node};
use serde::Deserialize;
use std::collections::HashSet;
use url::Url;

pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ARTICLES: usize = 10_000;
pub const MAX_STRING_BYTES: usize = 512 * 1024;
pub const MAX_XML_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedFormat {
    Rss,
    Atom,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImageSource {
    InlineHtml,
    MediaThumbnail,
    MediaContent,
    Enclosure,
    JsonImage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageCandidate {
    pub url: String,
    pub source: ImageSource,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub mime_type: Option<String>,
    pub alt_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedArticle {
    pub stable_id: String,
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub image: Option<ImageCandidate>,
    pub image_candidates: Vec<ImageCandidate>,
    pub enclosure_url: Option<String>,
    pub enclosure_type: Option<String>,
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFeed {
    pub format: FeedFormat,
    pub title: String,
    pub description: Option<String>,
    pub site_url: Option<String>,
    pub subscription_url: Option<String>,
    pub language: Option<String>,
    pub icon_candidates: Vec<String>,
    pub feed_image: Option<String>,
    pub articles: Vec<ParsedArticle>,
}

pub fn parse(input: &str) -> Result<ParsedFeed, CoreError> {
    parse_with_source(input, "https://invalid.example/feed")
}

pub fn parse_with_source(input: &str, source_url: &str) -> Result<ParsedFeed, CoreError> {
    if input.is_empty() {
        return Err(CoreError::UnusableFeed);
    }
    if input.len() > MAX_DOCUMENT_BYTES {
        return Err(CoreError::InputLimitExceeded("document bytes"));
    }
    let trimmed = input.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with('{') {
        parse_json(input, source_url)
    } else {
        parse_xml(input, source_url)
    }
}

fn parse_xml(input: &str, source: &str) -> Result<ParsedFeed, CoreError> {
    let document = Document::parse(input).map_err(|_| CoreError::MalformedXml)?;
    if document
        .descendants()
        .filter(Node::is_element)
        .any(|node| node.ancestors().count() > MAX_XML_DEPTH)
    {
        return Err(CoreError::InputLimitExceeded("XML nesting"));
    }
    let root = document.root_element();
    match root.tag_name().name().to_ascii_lowercase().as_str() {
        "rss" | "rdf" => parse_rss(root, source),
        "feed" => parse_atom(root, source),
        _ => Err(CoreError::UnsupportedFeed),
    }
}

fn parse_rss(root: Node<'_, '_>, source: &str) -> Result<ParsedFeed, CoreError> {
    let channel = descendants(root, "channel").next().unwrap_or(root);
    let title = clean_title(child_text(channel, "title"), source);
    let header_children = || {
        channel
            .children()
            .filter(Node::is_element)
            .filter(|n| name(*n) != "item")
    };
    let site_url = header_children()
        .find(|n| name(*n) == "link")
        .and_then(node_text)
        .and_then(|v| resolve(source, &v));
    let subscription_url = header_children()
        .find(|n| {
            name(*n) == "link" && attr(*n, "rel").is_some_and(|r| r.eq_ignore_ascii_case("self"))
        })
        .and_then(|n| attr(n, "href"))
        .and_then(|v| resolve(source, v));
    let image = descendants(channel, "image")
        .next()
        .and_then(|n| child_text(n, "url"))
        .and_then(|v| resolve(source, &v));
    let mut icons = Vec::new();
    if let Some(value) = image.clone() {
        icons.push(value);
    }
    let item_root = if name(root).eq_ignore_ascii_case("rdf") {
        root
    } else {
        channel
    };
    let items: Vec<_> = descendants(item_root, "item")
        .take(MAX_ARTICLES + 1)
        .collect();
    if items.len() > MAX_ARTICLES {
        return Err(CoreError::InputLimitExceeded("article count"));
    }
    let articles = deduplicate(
        items
            .into_iter()
            .map(|item| parse_xml_article(item, source, FeedFormat::Rss))
            .collect(),
    );
    Ok(ParsedFeed {
        format: FeedFormat::Rss,
        title,
        description: child_text(channel, "description").and_then(clean_optional),
        site_url,
        subscription_url,
        language: child_text(channel, "language").and_then(clean_optional),
        icon_candidates: icons,
        feed_image: image,
        articles,
    })
}

fn parse_atom(root: Node<'_, '_>, source: &str) -> Result<ParsedFeed, CoreError> {
    let title = clean_title(child_text(root, "title"), source);
    let site_url = atom_link(root, source, "alternate", Some("text/html"));
    let subscription_url = atom_link(root, source, "self", None);
    let mut icons = Vec::new();
    for key in ["icon", "logo"] {
        if let Some(url) = child_text(root, key).and_then(|v| resolve(source, &v))
            && !icons.contains(&url)
        {
            icons.push(url);
        }
    }
    let entries: Vec<_> = root
        .children()
        .filter(Node::is_element)
        .filter(|n| name(*n) == "entry")
        .take(MAX_ARTICLES + 1)
        .collect();
    if entries.len() > MAX_ARTICLES {
        return Err(CoreError::InputLimitExceeded("article count"));
    }
    let articles = deduplicate(
        entries
            .into_iter()
            .map(|entry| parse_xml_article(entry, source, FeedFormat::Atom))
            .collect(),
    );
    Ok(ParsedFeed {
        format: FeedFormat::Atom,
        title,
        description: child_text(root, "subtitle").and_then(clean_optional),
        site_url,
        subscription_url,
        language: attr(root, "lang").map(ToOwned::to_owned),
        feed_image: icons.first().cloned(),
        icon_candidates: icons,
        articles,
    })
}

fn parse_xml_article(node: Node<'_, '_>, source: &str, format: FeedFormat) -> ParsedArticle {
    let title_raw = child_text(node, "title");
    let title = title_raw
        .as_deref()
        .map(plain_text)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "Untitled Article".into());
    let explicit_id = child_text(
        node,
        if format == FeedFormat::Atom {
            "id"
        } else {
            "guid"
        },
    );
    let url = if format == FeedFormat::Atom {
        atom_link(node, source, "alternate", Some("text/html"))
    } else {
        child_text(node, "link").and_then(|v| resolve(source, &v))
    };
    let author = if format == FeedFormat::Atom {
        child(node, "author").and_then(|a| child_text(a, "name"))
    } else {
        child_text(node, "creator").or_else(|| child_text(node, "author"))
    }
    .and_then(clean_optional)
    .map(|v| plain_text(&v));
    let published_at = child_text(
        node,
        if format == FeedFormat::Atom {
            "published"
        } else {
            "pubdate"
        },
    )
    .and_then(|v| parse_date(&v));
    let updated_at = child_text(
        node,
        if format == FeedFormat::Atom {
            "updated"
        } else {
            "date"
        },
    )
    .and_then(|v| parse_date(&v));
    let published_at = published_at.or(updated_at);
    let summary_raw = child_text(
        node,
        if format == FeedFormat::Atom {
            "summary"
        } else {
            "description"
        },
    );
    let content = child_text(node, "encoded")
        .or_else(|| child_text(node, "content"))
        .and_then(clean_optional);
    let summary = summary_raw
        .as_deref()
        .or(content.as_deref())
        .map(plain_text)
        .and_then(clean_optional);
    let mut candidates = media_candidates(node, source);
    candidates.extend(inline_images(
        content.as_deref().or(summary_raw.as_deref()),
        source,
        url.as_deref(),
    ));
    let candidates = unique_candidates(candidates);
    let image = select_image(&candidates);
    let enclosure = node.children().find(|n| name(*n) == "enclosure");
    let enclosure_url = enclosure
        .and_then(|n| {
            attr(
                n,
                if format == FeedFormat::Atom {
                    "href"
                } else {
                    "url"
                },
            )
        })
        .and_then(|v| resolve(source, v));
    let enclosure_type = enclosure.and_then(|n| attr(n, "type")).map(str::to_owned);
    let categories = unique_strings(
        node.children()
            .filter(|n| name(*n) == "category")
            .filter_map(|n| attr(n, "term").map(str::to_owned).or_else(|| node_text(n))),
    );
    let stable_id = parser_stable_id(
        explicit_id.as_deref(),
        url.as_deref(),
        &title,
        published_at,
        author.as_deref(),
    );
    ParsedArticle {
        stable_id,
        title,
        url,
        author,
        published_at,
        updated_at,
        summary,
        content,
        image,
        image_candidates: candidates,
        enclosure_url,
        enclosure_type,
        categories,
    }
}

#[derive(Deserialize)]
struct JsonFeed {
    version: String,
    title: Option<String>,
    home_page_url: Option<String>,
    feed_url: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    favicon: Option<String>,
    language: Option<String>,
    #[serde(default)]
    items: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
struct JsonItem {
    id: String,
    url: Option<String>,
    external_url: Option<String>,
    title: Option<String>,
    content_html: Option<String>,
    content_text: Option<String>,
    summary: Option<String>,
    image: Option<String>,
    banner_image: Option<String>,
    date_published: Option<String>,
    date_modified: Option<String>,
    authors: Option<Vec<JsonAuthor>>,
    author: Option<JsonAuthor>,
    tags: Option<Vec<String>>,
    attachments: Option<Vec<JsonAttachment>>,
}
#[derive(Deserialize)]
struct JsonAuthor {
    name: Option<String>,
}
#[derive(Deserialize)]
struct JsonAttachment {
    url: String,
    mime_type: Option<String>,
}

fn parse_json(input: &str, source: &str) -> Result<ParsedFeed, CoreError> {
    let feed: JsonFeed = serde_json::from_str(input).map_err(|_| CoreError::MalformedJsonFeed)?;
    if !feed
        .version
        .to_ascii_lowercase()
        .contains("jsonfeed.org/version")
    {
        return Err(CoreError::UnsupportedFeed);
    }
    if feed.items.len() > MAX_ARTICLES {
        return Err(CoreError::InputLimitExceeded("article count"));
    }
    let mut articles = Vec::new();
    for value in feed.items {
        let Ok(item) = serde_json::from_value::<JsonItem>(value) else {
            continue;
        };
        let url = item
            .url
            .or(item.external_url)
            .and_then(|v| resolve(source, &v));
        let title = item
            .title
            .as_deref()
            .map(plain_text)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "Untitled Article".into());
        let published_at = item.date_published.as_deref().and_then(parse_date);
        let updated_at = item.date_modified.as_deref().and_then(parse_date);
        let content = item
            .content_html
            .or(item.content_text)
            .and_then(clean_optional);
        let summary = item
            .summary
            .and_then(clean_optional)
            .or_else(|| content.as_deref().map(plain_text).and_then(clean_optional));
        let author = item
            .authors
            .and_then(|v| {
                let names: Vec<_> = v.into_iter().filter_map(|a| a.name).collect();
                (!names.is_empty()).then(|| names.join(", "))
            })
            .or_else(|| item.author.and_then(|a| a.name));
        let mut candidates = Vec::new();
        for value in [item.image, item.banner_image].into_iter().flatten() {
            if let Some(url) = resolve(source, &value) {
                candidates.push(ImageCandidate {
                    url,
                    source: ImageSource::JsonImage,
                    width: None,
                    height: None,
                    mime_type: None,
                    alt_text: None,
                });
            }
        }
        let attachment = item.attachments.as_ref().and_then(|v| v.first());
        for a in item
            .attachments
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|a| {
                a.mime_type
                    .as_deref()
                    .is_some_and(|m| m.starts_with("image/"))
            })
        {
            if let Some(url) = resolve(source, &a.url) {
                candidates.push(ImageCandidate {
                    url,
                    source: ImageSource::Enclosure,
                    width: None,
                    height: None,
                    mime_type: a.mime_type.clone(),
                    alt_text: None,
                });
            }
        }
        candidates.extend(inline_images(content.as_deref(), source, url.as_deref()));
        let candidates = unique_candidates(candidates);
        let image = select_image(&candidates);
        let stable_id = parser_stable_id(
            Some(&item.id),
            url.as_deref(),
            &title,
            published_at,
            author.as_deref(),
        );
        articles.push(ParsedArticle {
            stable_id,
            title,
            url,
            author,
            published_at,
            updated_at,
            summary,
            content,
            image,
            image_candidates: candidates,
            enclosure_url: attachment.and_then(|a| resolve(source, &a.url)),
            enclosure_type: attachment.and_then(|a| a.mime_type.clone()),
            categories: item.tags.unwrap_or_default(),
        });
    }
    let mut icons = Vec::new();
    for value in [feed.icon, feed.favicon].into_iter().flatten() {
        if let Some(url) = resolve(source, &value)
            && !icons.contains(&url)
        {
            icons.push(url);
        }
    }
    Ok(ParsedFeed {
        format: FeedFormat::Json,
        title: clean_title(feed.title, source),
        description: feed.description.and_then(clean_optional),
        site_url: feed.home_page_url.and_then(|v| resolve(source, &v)),
        subscription_url: feed.feed_url.and_then(|v| resolve(source, &v)),
        language: feed.language,
        feed_image: icons.first().cloned(),
        icon_candidates: icons,
        articles: deduplicate(articles),
    })
}

pub fn parse_date(value: &str) -> Option<i64> {
    let value = value.trim();
    DateTime::parse_from_rfc3339(value)
        .map(|v| v.timestamp())
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc2822(value)
                .map(|v| v.timestamp())
                .ok()
        })
        .or_else(|| {
            [
                "%a, %d %b %Y %H:%M:%S %z",
                "%a, %e %b %Y %H:%M:%S %z",
                "%a, %d %b %Y %H:%M %z",
                "%d %b %Y %H:%M:%S %z",
                "%Y-%m-%d %H:%M:%S %z",
            ]
            .iter()
            .find_map(|f| {
                DateTime::parse_from_str(value, f)
                    .map(|v| v.timestamp())
                    .ok()
            })
        })
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|v| v.and_hms_opt(0, 0, 0))
                .map(|v| DateTime::<Utc>::from_naive_utc_and_offset(v, Utc).timestamp())
        })
}

fn media_candidates(node: Node<'_, '_>, source: &str) -> Vec<ImageCandidate> {
    let mut out = Vec::new();
    for child in node.descendants().filter(Node::is_element) {
        let local = name(child);
        let (image_source, value) = match local {
            "thumbnail" => (ImageSource::MediaThumbnail, attr(child, "url")),
            "content" if attr(child, "url").is_some() => {
                (ImageSource::MediaContent, attr(child, "url"))
            }
            "enclosure" if attr(child, "type").is_some_and(|v| v.starts_with("image/")) => (
                ImageSource::Enclosure,
                attr(child, "url").or_else(|| attr(child, "href")),
            ),
            _ => continue,
        };
        let medium = attr(child, "medium");
        let mime = attr(child, "type");
        if medium.is_some_and(|m| !m.eq_ignore_ascii_case("image"))
            || mime.is_some_and(|m| !m.starts_with("image/"))
        {
            continue;
        }
        if let Some(url) = value.and_then(|v| resolve(source, v)) {
            out.push(ImageCandidate {
                url,
                source: image_source,
                width: attr(child, "width").and_then(|v| v.parse().ok()),
                height: attr(child, "height").and_then(|v| v.parse().ok()),
                mime_type: mime.map(str::to_owned),
                alt_text: None,
            });
        }
    }
    out
}

fn inline_images(
    html: Option<&str>,
    feed_url: &str,
    article_url: Option<&str>,
) -> Vec<ImageCandidate> {
    let Some(html) = html else { return Vec::new() };
    let base = article_url.unwrap_or(feed_url);
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.to_ascii_lowercase().find("<img") {
        rest = &rest[at + 4..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        let Some(src) = html_attr(tag, "src").and_then(|v| resolve(base, &v)) else {
            continue;
        };
        out.push(ImageCandidate {
            url: src,
            source: ImageSource::InlineHtml,
            width: html_attr(tag, "width").and_then(|v| v.parse().ok()),
            height: html_attr(tag, "height").and_then(|v| v.parse().ok()),
            mime_type: None,
            alt_text: html_attr(tag, "alt"),
        });
    }
    out
}

fn select_image(candidates: &[ImageCandidate]) -> Option<ImageCandidate> {
    candidates
        .iter()
        .filter(|c| plausible_image(c))
        .max_by_key(|c| {
            (
                c.source,
                c.width.unwrap_or(0).saturating_mul(c.height.unwrap_or(0)),
            )
        })
        .cloned()
}
fn plausible_image(c: &ImageCandidate) -> bool {
    let lower = c.url.to_ascii_lowercase();
    if [
        "pixel",
        "tracker",
        "spacer",
        "clear.gif",
        "1x1",
        "avatar",
        "gravatar",
        "favicon",
        "logo",
    ]
    .iter()
    .any(|v| lower.contains(v))
    {
        return false;
    }
    if let (Some(w), Some(h)) = (c.width, c.height) {
        w >= 80 && h >= 60 && w.max(h) / w.min(h).max(1) <= 5
    } else {
        true
    }
}
fn unique_candidates(values: Vec<ImageCandidate>) -> Vec<ImageCandidate> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|v| seen.insert(crate::identity::normalize_url(&v.url)))
        .collect()
}
fn deduplicate(values: Vec<ParsedArticle>) -> Vec<ParsedArticle> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|v| seen.insert(v.stable_id.clone()))
        .collect()
}
fn unique_strings(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .filter(|v| !v.trim().is_empty() && seen.insert(v.clone()))
        .collect()
}
fn descendants<'a, 'input>(
    node: Node<'a, 'input>,
    key: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> {
    node.descendants()
        .filter(move |n| n.is_element() && name(*n) == key)
}
fn child<'a, 'input>(node: Node<'a, 'input>, key: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|n| n.is_element() && name(*n) == key)
}
fn child_text(node: Node<'_, '_>, key: &str) -> Option<String> {
    child(node, key).and_then(node_text)
}
fn node_text(node: Node<'_, '_>) -> Option<String> {
    let value = node.text()?.trim();
    (!value.is_empty() && value.len() <= MAX_STRING_BYTES).then(|| value.to_owned())
}
fn name<'input>(node: Node<'_, 'input>) -> &'input str {
    node.tag_name().name()
}
fn attr<'input>(node: Node<'input, 'input>, key: &str) -> Option<&'input str> {
    node.attributes()
        .find(|a| a.name().eq_ignore_ascii_case(key))
        .map(|a| a.value())
}
fn atom_link(
    node: Node<'_, '_>,
    source: &str,
    relation: &str,
    preferred_type: Option<&str>,
) -> Option<String> {
    let links: Vec<_> = node
        .children()
        .filter(|n| {
            name(*n) == "link"
                && attr(*n, "rel")
                    .unwrap_or("alternate")
                    .eq_ignore_ascii_case(relation)
        })
        .collect();
    links
        .iter()
        .find(|n| preferred_type.is_some_and(|t| attr(**n, "type") == Some(t)))
        .or_else(|| links.first())
        .and_then(|n| attr(*n, "href"))
        .and_then(|v| resolve(source, v))
}
fn resolve(base: &str, value: &str) -> Option<String> {
    let base = Url::parse(base).ok()?;
    let url = base.join(value.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}
fn clean_title(value: Option<String>, source: &str) -> String {
    value
        .as_deref()
        .map(plain_text)
        .filter(|v| !v.is_empty())
        .or_else(|| {
            Url::parse(source)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
        })
        .unwrap_or_else(|| "Untitled Feed".into())
}
fn clean_optional(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty() && value.len() <= MAX_STRING_BYTES).then_some(value)
}
fn plain_text(value: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for c in value.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                result.push(' ');
            }
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn html_attr(tag: &str, key: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{key}="))? + key.len() + 1;
    let tail = tag[at..].trim_start();
    let quote = tail.chars().next()?;
    if quote == '\'' || quote == '"' {
        Some(tail[1..].split(quote).next()?.to_owned())
    } else {
        Some(tail.split_whitespace().next()?.to_owned())
    }
}
fn parser_stable_id(
    explicit: Option<&str>,
    url: Option<&str>,
    title: &str,
    published: Option<i64>,
    author: Option<&str>,
) -> String {
    if let Some(value) = explicit.map(str::trim).filter(|v| !v.is_empty()) {
        return value.to_owned();
    }
    if let Some(value) = url {
        return format!("url:{}", crate::identity::normalize_url(value));
    }
    format!(
        "fallback:{}|{}|{}",
        title.to_lowercase(),
        published.map_or_else(|| "undated".into(), |v| v.to_string()),
        author.unwrap_or("").trim().to_lowercase()
    )
}
