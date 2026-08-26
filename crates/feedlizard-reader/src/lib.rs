use scraper::{Html, node::Node};
use std::{error::Error, fmt};
use url::Url;

pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_BLOCKS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading { level: u8, text: String },
    Paragraph { text: String, links: Vec<Link> },
    Quote(String),
    Code(String),
    ListItem(String),
    Image { url: String, alt: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub text: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageStyle {
    Heading(u8),
    Body,
    Quote,
    Code,
    ListItem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageChunk {
    Text { style: PageStyle, text: String },
    Image { url: String, alt: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub chunks: Vec<PageChunk>,
}

pub fn paginate(
    document: &Document,
    page_height: u32,
    block_gap: u32,
    image_height: u32,
    measure: impl Fn(&str, PageStyle) -> u32,
) -> Vec<Page> {
    let page_height = page_height.max(1);
    let mut pages = vec![Page { chunks: Vec::new() }];
    let mut used = 0_u32;
    for block in &document.blocks {
        match block {
            Block::Image { url, alt } => {
                if used > 0 && used.saturating_add(image_height + block_gap) > page_height {
                    pages.push(Page { chunks: Vec::new() });
                    used = 0;
                }
                pages
                    .last_mut()
                    .expect("page exists")
                    .chunks
                    .push(PageChunk::Image {
                        url: url.clone(),
                        alt: alt.clone(),
                    });
                used = used.saturating_add(image_height.min(page_height) + block_gap);
            }
            _ => {
                let (text, style) = page_text(block);
                append_paginated_text(
                    text,
                    style,
                    page_height,
                    block_gap,
                    &measure,
                    &mut pages,
                    &mut used,
                );
            }
        }
    }
    pages.retain(|page| !page.chunks.is_empty());
    if pages.is_empty() {
        pages.push(Page { chunks: Vec::new() });
    }
    pages
}

fn page_text(block: &Block) -> (&str, PageStyle) {
    match block {
        Block::Heading { level, text } => (text, PageStyle::Heading(*level)),
        Block::Paragraph { text, .. } => (text, PageStyle::Body),
        Block::Quote(text) => (text, PageStyle::Quote),
        Block::Code(text) => (text, PageStyle::Code),
        Block::ListItem(text) => (text, PageStyle::ListItem),
        Block::Image { .. } => unreachable!("images are handled separately"),
    }
}

fn append_paginated_text(
    text: &str,
    style: PageStyle,
    page_height: u32,
    gap: u32,
    measure: &impl Fn(&str, PageStyle) -> u32,
    pages: &mut Vec<Page>,
    used: &mut u32,
) {
    let words = text.split_whitespace().collect::<Vec<_>>();
    let mut start = 0;
    while start < words.len() {
        let available = page_height.saturating_sub(*used).saturating_sub(gap);
        let mut low = start + 1;
        let mut high = words.len();
        let mut best = None;
        while low <= high {
            let middle = low + (high - low) / 2;
            let candidate = words[start..middle].join(" ");
            if measure(&candidate, style) <= available.max(1) {
                best = Some((middle, candidate));
                low = middle + 1;
            } else {
                high = middle.saturating_sub(1);
            }
        }
        let (end, chunk) = best.unwrap_or_else(|| (start + 1, words[start].to_owned()));
        let height = measure(&chunk, style).min(page_height);
        if *used > 0 && height.saturating_add(gap) > page_height.saturating_sub(*used) {
            pages.push(Page { chunks: Vec::new() });
            *used = 0;
            continue;
        }
        pages
            .last_mut()
            .expect("page exists")
            .chunks
            .push(PageChunk::Text { style, text: chunk });
        *used = used.saturating_add(height + gap);
        start = end;
        if start < words.len() {
            pages.push(Page { chunks: Vec::new() });
            *used = 0;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReaderError {
    InputTooLarge,
    TooManyBlocks,
    InvalidBaseUrl,
}

impl fmt::Display for ReaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge => write!(formatter, "article HTML exceeds the reader limit"),
            Self::TooManyBlocks => write!(formatter, "article contains too many blocks"),
            Self::InvalidBaseUrl => write!(formatter, "article base URL is invalid"),
        }
    }
}

impl Error for ReaderError {}

pub fn parse_feed_html(input: &str, base_url: Option<&str>) -> Result<Document, ReaderError> {
    if input.len() > MAX_HTML_BYTES {
        return Err(ReaderError::InputTooLarge);
    }
    let base = base_url
        .map(Url::parse)
        .transpose()
        .map_err(|_| ReaderError::InvalidBaseUrl)?;
    let html = Html::parse_fragment(input);
    let mut blocks = Vec::new();
    for child in html.tree.root().children() {
        collect_blocks(child, base.as_ref(), &mut blocks)?;
    }
    Ok(Document { blocks })
}

fn collect_blocks(
    node: ego_tree::NodeRef<'_, Node>,
    base: Option<&Url>,
    blocks: &mut Vec<Block>,
) -> Result<(), ReaderError> {
    if blocks.len() >= MAX_BLOCKS {
        return Err(ReaderError::TooManyBlocks);
    }
    let Some(element) = node.value().as_element() else {
        return Ok(());
    };
    let name = element.name();
    if matches!(
        name,
        "script" | "style" | "noscript" | "iframe" | "object" | "embed"
    ) {
        return Ok(());
    }
    let text = || {
        clean_text(
            &node
                .descendants()
                .filter_map(|child| child.value().as_text().map(|value| value.as_ref()))
                .collect::<Vec<_>>()
                .join(" "),
        )
    };
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => push_nonempty(
            blocks,
            Block::Heading {
                level: name[1..].parse().unwrap_or(2),
                text: text(),
            },
        ),
        "p" => {
            let content = text();
            if !content.is_empty() {
                blocks.push(Block::Paragraph {
                    text: content,
                    links: collect_links(node, base),
                });
            }
        }
        "blockquote" => push_nonempty(blocks, Block::Quote(text())),
        "pre" => push_nonempty(blocks, Block::Code(text())),
        "li" => push_nonempty(blocks, Block::ListItem(text())),
        "img" => {
            if let Some(source) = element.attr("src").and_then(|value| safe_url(value, base)) {
                blocks.push(Block::Image {
                    url: source,
                    alt: clean_text(element.attr("alt").unwrap_or("Article image")),
                });
            }
        }
        "article" | "main" | "section" | "div" | "body" | "html" | "ul" | "ol" => {
            for child in node.children() {
                collect_blocks(child, base, blocks)?;
            }
        }
        _ => {
            for child in node.children() {
                collect_blocks(child, base, blocks)?;
            }
        }
    }
    Ok(())
}

fn collect_links(node: ego_tree::NodeRef<'_, Node>, base: Option<&Url>) -> Vec<Link> {
    node.descendants()
        .filter_map(|child| {
            let element = child.value().as_element()?;
            if element.name() != "a" {
                return None;
            }
            let url = safe_url(element.attr("href")?, base)?;
            let text = clean_text(
                &child
                    .descendants()
                    .filter_map(|item| item.value().as_text().map(|value| value.as_ref()))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            (!text.is_empty()).then_some(Link { text, url })
        })
        .collect()
}

fn safe_url(value: &str, base: Option<&Url>) -> Option<String> {
    let parsed = Url::parse(value).ok().or_else(|| base?.join(value).ok())?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.into())
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_nonempty(blocks: &mut Vec<Block>, block: Block) {
    let empty = match &block {
        Block::Heading { text, .. }
        | Block::Quote(text)
        | Block::Code(text)
        | Block::ListItem(text) => text.is_empty(),
        _ => false,
    };
    if !empty {
        blocks.push(block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_semantics_and_resolves_safe_links() {
        let document = parse_feed_html("<h2>Heading</h2><p>Hello <a href='/story'>world</a>.</p><blockquote>Worth reading</blockquote>", Some("https://example.com/news/")).unwrap();
        assert_eq!(document.blocks.len(), 3);
        assert!(
            matches!(&document.blocks[0], Block::Heading { level: 2, text } if text == "Heading")
        );
        assert!(
            matches!(&document.blocks[1], Block::Paragraph { links, .. } if links[0].url == "https://example.com/story")
        );
    }

    #[test]
    fn drops_executable_content_and_dangerous_schemes() {
        let document = parse_feed_html("<script>alert(1)</script><p>Safe <a href='javascript:bad()'>link</a></p><img src='data:text/plain,no'>", None).unwrap();
        assert_eq!(
            document.blocks,
            vec![Block::Paragraph {
                text: "Safe link".into(),
                links: vec![]
            }]
        );
    }

    #[test]
    fn rejects_oversized_input() {
        assert_eq!(
            parse_feed_html(&"x".repeat(MAX_HTML_BYTES + 1), None),
            Err(ReaderError::InputTooLarge)
        );
    }

    #[test]
    fn pagination_is_deterministic_and_splits_oversized_paragraphs() {
        let document = Document {
            blocks: vec![Block::Paragraph {
                text: "one two three four five six seven eight".into(),
                links: vec![],
            }],
        };
        let measure = |text: &str, _: PageStyle| text.len() as u32;
        let first = paginate(&document, 12, 1, 8, measure);
        let second = paginate(&document, 12, 1, 8, measure);
        assert_eq!(first, second);
        assert!(first.len() >= 3);
        assert!(first.iter().all(|page| !page.chunks.is_empty()));
    }
}
