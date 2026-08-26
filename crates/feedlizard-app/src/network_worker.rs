use feedlizard_network::{CancellationToken, FetchPolicy, HttpClient};
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
}

#[derive(Debug)]
pub enum Event {
    FeedAdded { feed_id: String, articles: usize },
    DiscoveryCandidates(Vec<(String, Option<String>)>),
    FeedRefreshed { inserted: usize, failed: bool },
    RefreshComplete(RefreshSummary),
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
    let coordinator = RefreshCoordinator::new(client, RefreshConfig::default());
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
        };
        let _ = events.send(event);
    }
}
