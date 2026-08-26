use feedlizard_core::{identity::feed_id, opml};
use feedlizard_nostr_backup::{
    BackupSnapshot, KeyIdentity, RelayClient, SecureKeyStore, create_backup_event, generate_key,
};
use feedlizard_storage::Library;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
];

#[derive(Debug)]
pub enum Command {
    Status,
    Generate,
    StoreGenerated(String),
    UseExisting(String),
    BackUpNow,
    FindRestore,
    PreviewRestore(String),
    ConfirmRestore,
    RemoveKey,
}

#[derive(Debug)]
pub enum Event {
    Status(Option<KeyIdentity>),
    Generated {
        nsec: String,
        identity: KeyIdentity,
    },
    Configured(KeyIdentity),
    BackupComplete {
        successful: usize,
        failed: usize,
    },
    RestoreHistory(Vec<SnapshotSummary>),
    RestorePreview {
        created_at: i64,
        subscriptions: usize,
        folders: usize,
        feeds_to_add: usize,
        feeds_already_present: usize,
    },
    RestoreComplete {
        added: usize,
        duplicates: usize,
    },
    KeyRemoved,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSummary {
    pub event_id: String,
    pub created_at: i64,
    pub subscriptions: usize,
    pub folders: usize,
}

#[derive(Clone)]
pub struct NostrWorker {
    sender: Sender<Command>,
}

impl NostrWorker {
    pub fn start(database_path: PathBuf) -> (Self, Receiver<Event>) {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("feedlizard-nostr-backup".into())
            .spawn(move || run(database_path, command_receiver, event_sender))
            .expect("Nostr backup worker starts");
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
            let _ = events.send(Event::Error(error.to_string()));
            return;
        }
    };
    let mut history: HashMap<String, BackupSnapshot> = HashMap::new();
    let mut pending_restore: Option<String> = None;
    while let Ok(command) = commands.recv() {
        let event = runtime.block_on(handle(
            &database_path,
            &mut history,
            &mut pending_restore,
            command,
        ));
        let _ = events.send(event.unwrap_or_else(Event::Error));
    }
}

async fn handle(
    database_path: &PathBuf,
    history: &mut HashMap<String, BackupSnapshot>,
    pending_restore: &mut Option<String>,
    command: Command,
) -> Result<Event, String> {
    match command {
        Command::Status => {
            let store = SecureKeyStore::open()
                .await
                .map_err(|error| error.to_string())?;
            store
                .identity()
                .await
                .map(Event::Status)
                .map_err(|error| error.to_string())
        }
        Command::Generate => generate_key()
            .map(|generated| Event::Generated {
                nsec: generated.nsec,
                identity: generated.identity,
            })
            .map_err(|error| error.to_string()),
        Command::StoreGenerated(nsec) | Command::UseExisting(nsec) => {
            let store = SecureKeyStore::open()
                .await
                .map_err(|error| error.to_string())?;
            store
                .store(&nsec)
                .await
                .map(Event::Configured)
                .map_err(|error| error.to_string())
        }
        Command::BackUpNow => {
            let store = SecureKeyStore::open()
                .await
                .map_err(|error| error.to_string())?;
            let nsec = store.load().await.map_err(|error| error.to_string())?;
            let library = Library::open(database_path).map_err(|error| error.to_string())?;
            let now = unix_now();
            let opml = library
                .export_opml("FeedLizard Nostr backup")
                .map_err(|error| error.to_string())?;
            let event =
                create_backup_event(&nsec, &opml, now).map_err(|error| error.to_string())?;
            let client = RelayClient::new(default_relays()).map_err(|error| error.to_string())?;
            let result = client
                .publish(&event)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Event::BackupComplete {
                successful: result.successful_relays,
                failed: result.failed_relays,
            })
        }
        Command::FindRestore => {
            let store = SecureKeyStore::open()
                .await
                .map_err(|error| error.to_string())?;
            let nsec = store.load().await.map_err(|error| error.to_string())?;
            let client = RelayClient::new(default_relays()).map_err(|error| error.to_string())?;
            let backups = client
                .fetch_history(&nsec)
                .await
                .map_err(|error| error.to_string())?;
            let summaries = backups
                .iter()
                .map(|backup| SnapshotSummary {
                    event_id: backup.event_id.clone(),
                    created_at: backup.created_at,
                    subscriptions: backup.feed_count,
                    folders: backup.folder_count,
                })
                .collect::<Vec<_>>();
            *history = backups
                .into_iter()
                .map(|backup| (backup.event_id.clone(), backup))
                .collect();
            *pending_restore = None;
            Ok(Event::RestoreHistory(summaries))
        }
        Command::PreviewRestore(event_id) => {
            let backup = history
                .get(&event_id)
                .ok_or_else(|| "The selected backup is no longer available".to_owned())?;
            let parsed = opml::import(&backup.opml).map_err(|error| error.to_string())?;
            let library = Library::open(database_path).map_err(|error| error.to_string())?;
            let existing = library
                .list_feeds()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|feed| feed.stable_id)
                .collect::<std::collections::HashSet<_>>();
            let feeds_to_add = parsed
                .feeds
                .iter()
                .filter(|feed| !existing.contains(&feed_id(&feed.feed_url)))
                .count();
            let feeds_already_present = parsed.feeds.len().saturating_sub(feeds_to_add);
            *pending_restore = Some(backup.opml.clone());
            Ok(Event::RestorePreview {
                created_at: backup.created_at,
                subscriptions: backup.feed_count,
                folders: backup.folder_count,
                feeds_to_add,
                feeds_already_present,
            })
        }
        Command::ConfirmRestore => {
            let opml = pending_restore
                .take()
                .ok_or_else(|| "No validated restore is pending".to_owned())?;
            let mut library = Library::open(database_path).map_err(|error| error.to_string())?;
            let stats = library
                .import_opml(&opml, unix_now())
                .map_err(|error| error.to_string())?;
            Ok(Event::RestoreComplete {
                added: stats.feeds_added,
                duplicates: stats.duplicates,
            })
        }
        Command::RemoveKey => {
            let store = SecureKeyStore::open()
                .await
                .map_err(|error| error.to_string())?;
            store.remove().await.map_err(|error| error.to_string())?;
            history.clear();
            *pending_restore = None;
            Ok(Event::KeyRemoved)
        }
    }
}

fn default_relays() -> Vec<String> {
    DEFAULT_RELAYS
        .iter()
        .map(|relay| (*relay).to_owned())
        .collect()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
