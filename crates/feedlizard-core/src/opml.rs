use crate::{error::CoreError, identity::normalize_url, parser::FeedFormat};
use roxmltree::{Document, Node};
use url::Url;

pub const MAX_OPML_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_OPML_FEEDS: usize = 10_000;
pub const MAX_FOLDER_DEPTH: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpmlFeed {
    pub title: String,
    pub feed_url: String,
    pub site_url: Option<String>,
    pub folders: Vec<String>,
    pub format: FeedFormat,
    pub custom_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpmlLibrary {
    pub feeds: Vec<OpmlFeed>,
    pub failures: Vec<String>,
}

pub fn import(input: &str) -> Result<OpmlLibrary, CoreError> {
    if input.is_empty() {
        return Err(CoreError::Opml("empty document".into()));
    }
    if input.len() > MAX_OPML_BYTES {
        return Err(CoreError::InputLimitExceeded("OPML document bytes"));
    }
    let document = Document::parse(input).map_err(|_| CoreError::Opml("malformed XML".into()))?;
    let root = document.root_element();
    if !root.tag_name().name().eq_ignore_ascii_case("opml") {
        return Err(CoreError::Opml("unsupported document".into()));
    }
    let body = root
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case("body"))
        .ok_or_else(|| CoreError::Opml("missing body".into()))?;
    let mut library = OpmlLibrary::default();
    for outline in body
        .children()
        .filter(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case("outline"))
    {
        walk(outline, &[], &mut library)?;
    }
    if library.feeds.is_empty() && library.failures.is_empty() {
        return Err(CoreError::Opml("no subscriptions".into()));
    }
    Ok(library)
}

fn walk(
    node: Node<'_, '_>,
    folders: &[String],
    library: &mut OpmlLibrary,
) -> Result<(), CoreError> {
    if folders.len() > MAX_FOLDER_DEPTH {
        return Err(CoreError::InputLimitExceeded("OPML folder depth"));
    }
    if library.feeds.len() >= MAX_OPML_FEEDS {
        return Err(CoreError::InputLimitExceeded("OPML feed count"));
    }
    let title = attribute(node, "title")
        .or_else(|| attribute(node, "text"))
        .unwrap_or("")
        .trim()
        .to_owned();
    if let Some(raw_url) = attribute(node, "xmlurl")
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        match web_url(raw_url) {
            Some(feed_url) => {
                let site_url = attribute(node, "htmlurl").and_then(web_url);
                let format = match attribute(node, "type")
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "atom" => FeedFormat::Atom,
                    "json" | "jsonfeed" => FeedFormat::Json,
                    _ => FeedFormat::Rss,
                };
                let resolved_title = if title.is_empty() {
                    Url::parse(&feed_url)
                        .ok()
                        .and_then(|u| u.host_str().map(str::to_owned))
                        .unwrap_or_else(|| feed_url.clone())
                } else {
                    title.clone()
                };
                let custom_title = attribute(node, "feedlizard:customtitle")
                    .or_else(|| attribute(node, "customtitle"))
                    .map(str::to_owned);
                library.feeds.push(OpmlFeed {
                    title: resolved_title,
                    feed_url,
                    site_url,
                    folders: folders.to_vec(),
                    format,
                    custom_title,
                });
            }
            None => library.failures.push(format!(
                "{}: invalid feed URL",
                if title.is_empty() {
                    "Untitled feed"
                } else {
                    &title
                }
            )),
        }
        return Ok(());
    }
    let kind = attribute(node, "type").unwrap_or("").to_ascii_lowercase();
    if matches!(kind.as_str(), "rss" | "atom" | "feed" | "xml" | "json") {
        library.failures.push(format!(
            "{}: missing feed URL",
            if title.is_empty() {
                "Untitled feed"
            } else {
                &title
            }
        ));
        return Ok(());
    }
    let mut next = folders.to_vec();
    if !title.is_empty() {
        next.push(title);
    }
    for child in node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case("outline"))
    {
        walk(child, &next, library)?;
    }
    Ok(())
}

pub fn export(library: &OpmlLibrary, created_rfc2822: &str) -> String {
    let mut root = FolderNode::default();
    let mut feeds = library.feeds.clone();
    feeds.sort_by(|a, b| a.feed_url.cmp(&b.feed_url));
    for feed in feeds {
        root.insert(feed);
    }
    let mut output = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\" xmlns:feedlizard=\"https://feedlizard.app/opml/1\"><head><title>FeedLizard Subscriptions</title><dateCreated>{}</dateCreated></head><body>\n",
        escape(created_rfc2822)
    );
    root.write(&mut output, 1);
    output.push_str("</body></opml>\n");
    output
}

#[derive(Default)]
struct FolderNode {
    feeds: Vec<OpmlFeed>,
    folders: std::collections::BTreeMap<String, FolderNode>,
}
impl FolderNode {
    fn insert(&mut self, mut feed: OpmlFeed) {
        if feed.folders.is_empty() {
            self.feeds.push(feed);
        } else {
            let first = feed.folders.remove(0);
            self.folders.entry(first).or_default().insert(feed);
        }
    }
    fn write(&self, output: &mut String, depth: usize) {
        let indent = "  ".repeat(depth);
        for feed in &self.feeds {
            let kind = match feed.format {
                FeedFormat::Rss => "rss",
                FeedFormat::Atom => "atom",
                FeedFormat::Json => "json",
            };
            output.push_str(&format!(
                "{indent}<outline text=\"{}\" title=\"{}\" type=\"{kind}\" xmlUrl=\"{}\"",
                escape(&feed.title),
                escape(&feed.title),
                escape(&feed.feed_url)
            ));
            if let Some(site) = &feed.site_url {
                output.push_str(&format!(" htmlUrl=\"{}\"", escape(site)));
            }
            if let Some(custom) = &feed.custom_title {
                output.push_str(&format!(" feedlizard:customTitle=\"{}\"", escape(custom)));
            }
            output.push_str(" />\n");
        }
        for (name, folder) in &self.folders {
            output.push_str(&format!(
                "{indent}<outline text=\"{}\" title=\"{}\">\n",
                escape(name),
                escape(name)
            ));
            folder.write(output, depth + 1);
            output.push_str(&format!("{indent}</outline>\n"));
        }
    }
}

fn attribute<'input>(node: Node<'input, 'input>, key: &str) -> Option<&'input str> {
    node.attributes()
        .find(|a| {
            a.name().eq_ignore_ascii_case(key)
                || (key.eq_ignore_ascii_case("feedlizard:customtitle")
                    && a.name().eq_ignore_ascii_case("customtitle"))
        })
        .map(|a| a.value())
}
fn web_url(value: &str) -> Option<String> {
    let url = Url::parse(value.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| normalize_url(url.as_str()))
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
