use feedlizard_network::{CancellationToken, FetchPolicy, HttpClient};
use feedlizard_reader::Document;
use feedlizard_refresh::{AddFeedResult, RefreshConfig, RefreshCoordinator, RefreshSummary};
use feedlizard_storage::Library;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

#[derive(Debug)]
pub enum Command {
    AddFeed(String),
    RefreshFeed(String),
    RefreshAll,
    DiscoverArticleImages(Vec<(String, String)>),
    DiscoverFavicons(Vec<(String, String)>),
    ExtractArticle { article_id: String, url: String },
}

#[derive(Debug)]
pub enum Event {
    FeedAdded {
        feed_id: String,
        articles: usize,
    },
    DiscoveryCandidates(Vec<(String, Option<String>)>),
    FeedRefreshed {
        inserted: usize,
        failed: bool,
    },
    RefreshComplete(RefreshSummary),
    ArticleImagesDiscovered(Vec<(String, String)>),
    FaviconsDiscovered(Vec<(String, String)>),
    ArticleExtracted {
        article_id: String,
        document: Document,
    },
    ArticleExtractionFailed {
        article_id: String,
        error: String,
    },
    Error(String),
}

#[derive(Clone)]
pub struct NetworkWorker {
    sender: Sender<Command>,
}

impl NetworkWorker {
    pub fn start(database_path: PathBuf) -> (Self, Receiver<Event>) {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("feedlizard-network".into())
            .spawn(move || run(database_path, command_receiver, event_sender))
            .expect("network worker starts");
        (
            Self {
                sender: command_sender,
            },
            event_receiver,
        )
    }

    pub fn send(&self, command: Command) {
        let _ = self.sender.send(command);
    }
}

fn run(database_path: PathBuf, commands: Receiver<Command>, events: Sender<Event>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = events.send(Event::Error(format!("Network runtime failed: {error}")));
            return;
        }
    };
    let client = match HttpClient::new(FetchPolicy::default()) {
        Ok(client) => client,
        Err(error) => {
            let _ = events.send(Event::Error(error.to_string()));
            return;
        }
    };
    let coordinator = RefreshCoordinator::new(client.clone(), RefreshConfig::default());
    let mut library = match Library::open(database_path) {
        Ok(library) => library,
        Err(error) => {
            let _ = events.send(Event::Error(error.to_string()));
            return;
        }
    };
    while let Ok(command) = commands.recv() {
        let token = CancellationToken::default();
        let event = match command {
            Command::AddFeed(url) => {
                match runtime.block_on(coordinator.add_url(&mut library, &url, &token)) {
                    Ok(AddFeedResult::Added { feed_id, ingest }) => Event::FeedAdded {
                        feed_id,
                        articles: ingest.inserted,
                    },
                    Ok(AddFeedResult::Candidates(result)) => Event::DiscoveryCandidates(
                        result
                            .candidates
                            .into_iter()
                            .map(|candidate| (candidate.url, candidate.title_hint))
                            .collect(),
                    ),
                    Err(error) => Event::Error(error.to_string()),
                }
            }
            Command::RefreshFeed(feed_id) => {
                match runtime.block_on(coordinator.refresh_one(&mut library, &feed_id, &token)) {
                    Ok(result) => Event::FeedRefreshed {
                        inserted: result.inserted,
                        failed: matches!(result.state, feedlizard_refresh::RefreshState::Failed),
                    },
                    Err(error) => Event::Error(error.to_string()),
                }
            }
            Command::RefreshAll => {
                match runtime.block_on(coordinator.refresh_all(&mut library, &token)) {
                    Ok(result) => Event::RefreshComplete(result.summary),
                    Err(error) => Event::Error(error.to_string()),
                }
            }
            Command::DiscoverArticleImages(candidates) => {
                let images =
                    runtime.block_on(coordinator.discover_article_images(candidates, &token));
                for (article_id, image_url) in &images {
                    if let Err(error) = library.set_article_image(article_id, image_url) {
                        eprintln!("Could not persist discovered article image: {error}");
                    }
                }
                Event::ArticleImagesDiscovered(images)
            }
            Command::DiscoverFavicons(candidates) => {
                let client = client.clone();
                let icons = runtime.block_on(async move {
                    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(6));
                    let mut tasks = tokio::task::JoinSet::new();
                    for (feed_id, site_url) in candidates.into_iter().take(100) {
                        let client = client.clone();
                        let semaphore = semaphore.clone();
                        tasks.spawn(async move {
                            let _permit = semaphore.acquire_owned().await.ok()?;
                            let token = CancellationToken::default();
                            client
                                .discover_site_icon(&site_url, &token)
                                .await
                                .ok()
                                .flatten()
                                .map(|url| (feed_id, url))
                        });
                    }
                    let mut icons = Vec::new();
                    while let Some(result) = tasks.join_next().await {
                        if let Ok(Some(icon)) = result {
                            icons.push(icon);
                        }
                    }
                    icons
                });
                for (feed_id, icon_url) in &icons {
                    if let Err(error) = library.set_feed_favicon(feed_id, icon_url) {
                        eprintln!("Could not persist discovered favicon: {error}");
                    }
                }
                Event::FaviconsDiscovered(icons)
            }
            Command::ExtractArticle { article_id, url } => {
                match runtime.block_on(client.fetch_article_html(&url, &token)) {
                    Ok(response) => match feedlizard_reader::extract_article(
                        &response.body,
                        &response.final_url,
                    ) {
                        Ok(document) if !document.blocks.is_empty() => Event::ArticleExtracted {
                            article_id,
                            document,
                        },
                        Ok(_) => Event::ArticleExtractionFailed {
                            article_id,
                            error: "The full article did not contain readable content".into(),
                        },
                        Err(error) => Event::ArticleExtractionFailed {
                            article_id,
                            error: format!("Could not extract full article: {error}"),
                        },
                    },
                    Err(error) => Event::ArticleExtractionFailed {
                        article_id,
                        error: format!("Could not load full article: {error}"),
                    },
                }
            }
        };
        let _ = events.send(event);
    }
}
