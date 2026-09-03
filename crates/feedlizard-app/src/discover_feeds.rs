use feedlizard_core::identity::feed_id;
use feedlizard_storage::FeedRecord;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub id: &'static str,
    pub name: &'static str,
    pub feed_url: &'static str,
    pub website_url: &'static str,
    pub category: &'static str,
}

pub const CATEGORIES: [&str; 6] = [
    "Technology",
    "Apple",
    "Linux & Open Source",
    "Security",
    "Science",
    "General News",
];

/// This release-bundled directory mirrors FeedLizard for Apple. Merely opening
/// Discover Feeds performs no network requests; selected publishers are only
/// contacted after the user presses Add.
pub const ENTRIES: [Entry; 21] = [
    Entry {
        id: "ars-technica",
        name: "Ars Technica",
        feed_url: "https://feeds.arstechnica.com/arstechnica/index",
        website_url: "https://arstechnica.com",
        category: "Technology",
    },
    Entry {
        id: "the-verge",
        name: "The Verge",
        feed_url: "https://www.theverge.com/rss/index.xml",
        website_url: "https://www.theverge.com",
        category: "Technology",
    },
    Entry {
        id: "techcrunch",
        name: "TechCrunch",
        feed_url: "https://techcrunch.com/feed/",
        website_url: "https://techcrunch.com",
        category: "Technology",
    },
    Entry {
        id: "wired",
        name: "WIRED",
        feed_url: "https://www.wired.com/feed/rss",
        website_url: "https://www.wired.com",
        category: "Technology",
    },
    Entry {
        id: "nerds-xyz",
        name: "NERDS.xyz",
        feed_url: "https://nerds.xyz/feed/",
        website_url: "https://nerds.xyz",
        category: "Technology",
    },
    Entry {
        id: "joanna-stern-new-things",
        name: "Joanna Stern: The New Things",
        feed_url: "https://rss.beehiiv.com/feeds/1vJYynypCP.xml",
        website_url: "https://thenewthings.com",
        category: "Technology",
    },
    Entry {
        id: "macrumors",
        name: "MacRumors",
        feed_url: "https://feeds.macrumors.com/MacRumors-All",
        website_url: "https://www.macrumors.com",
        category: "Apple",
    },
    Entry {
        id: "9to5mac",
        name: "9to5Mac",
        feed_url: "https://9to5mac.com/feed/",
        website_url: "https://9to5mac.com",
        category: "Apple",
    },
    Entry {
        id: "apple-newsroom",
        name: "Apple Newsroom",
        feed_url: "https://www.apple.com/newsroom/rss-feed.rss",
        website_url: "https://www.apple.com/newsroom/",
        category: "Apple",
    },
    Entry {
        id: "phoronix",
        name: "Phoronix",
        feed_url: "https://www.phoronix.com/rss.php",
        website_url: "https://www.phoronix.com",
        category: "Linux & Open Source",
    },
    Entry {
        id: "fedora-magazine",
        name: "Fedora Magazine",
        feed_url: "https://fedoramagazine.org/feed/",
        website_url: "https://fedoramagazine.org",
        category: "Linux & Open Source",
    },
    Entry {
        id: "omg-ubuntu",
        name: "OMG! Ubuntu",
        feed_url: "https://www.omgubuntu.co.uk/feed",
        website_url: "https://www.omgubuntu.co.uk",
        category: "Linux & Open Source",
    },
    Entry {
        id: "ubuntu-blog",
        name: "Ubuntu Blog",
        feed_url: "https://ubuntu.com/blog/feed",
        website_url: "https://ubuntu.com/blog",
        category: "Linux & Open Source",
    },
    Entry {
        id: "krebs-security",
        name: "Krebs on Security",
        feed_url: "https://krebsonsecurity.com/feed/",
        website_url: "https://krebsonsecurity.com",
        category: "Security",
    },
    Entry {
        id: "bleeping-computer",
        name: "BleepingComputer",
        feed_url: "https://www.bleepingcomputer.com/feed/",
        website_url: "https://www.bleepingcomputer.com",
        category: "Security",
    },
    Entry {
        id: "cisa-advisories",
        name: "CISA Cybersecurity Advisories",
        feed_url: "https://www.cisa.gov/cybersecurity-advisories/all.xml",
        website_url: "https://www.cisa.gov/news-events/cybersecurity-advisories",
        category: "Security",
    },
    Entry {
        id: "nasa",
        name: "NASA",
        feed_url: "https://www.nasa.gov/feed/",
        website_url: "https://www.nasa.gov",
        category: "Science",
    },
    Entry {
        id: "science-daily",
        name: "ScienceDaily",
        feed_url: "https://www.sciencedaily.com/rss/all.xml",
        website_url: "https://www.sciencedaily.com",
        category: "Science",
    },
    Entry {
        id: "nature",
        name: "Nature",
        feed_url: "https://www.nature.com/nature.rss",
        website_url: "https://www.nature.com",
        category: "Science",
    },
    Entry {
        id: "bbc-news",
        name: "BBC News",
        feed_url: "https://feeds.bbci.co.uk/news/rss.xml",
        website_url: "https://www.bbc.com/news",
        category: "General News",
    },
    Entry {
        id: "npr-news",
        name: "NPR News",
        feed_url: "https://feeds.npr.org/1001/rss.xml",
        website_url: "https://www.npr.org",
        category: "General News",
    },
];

pub fn subscribed_ids(feeds: &[FeedRecord]) -> HashSet<String> {
    feeds.iter().map(|feed| feed.stable_id.clone()).collect()
}

pub fn is_subscribed(entry: &Entry, subscribed: &HashSet<String>) -> bool {
    subscribed.contains(&feed_id(entry.feed_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_ids_and_urls_are_unique_and_categories_are_known() {
        let ids = ENTRIES.iter().map(|entry| entry.id).collect::<HashSet<_>>();
        let urls = ENTRIES
            .iter()
            .map(|entry| entry.feed_url)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), ENTRIES.len());
        assert_eq!(urls.len(), ENTRIES.len());
        assert!(
            ENTRIES
                .iter()
                .all(|entry| CATEGORIES.contains(&entry.category))
        );
        assert!(ENTRIES.iter().all(|entry| {
            url::Url::parse(entry.feed_url).is_ok() && url::Url::parse(entry.website_url).is_ok()
        }));
    }

    #[test]
    fn mirrors_the_apple_release_directory() {
        assert_eq!(ENTRIES.len(), 21);
        assert!(ENTRIES.iter().any(|entry| entry.id == "nerds-xyz"));
        assert_eq!(CATEGORIES.len(), 6);
    }
}
