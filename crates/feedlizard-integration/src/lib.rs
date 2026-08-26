use feedlizard_storage::{ArticleScope, Library};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};
use thiserror::Error;
use tokio::sync::mpsc;
use zbus::{Connection, fdo, object_server::SignalEmitter};

pub const SERVICE_NAME: &str = "io.github.feedlizard.FeedLizard.Integration";
pub const OBJECT_PATH: &str = "/io/github/feedlizard/FeedLizard/Integration";
pub const INTERFACE_NAME: &str = "io.github.feedlizard.FeedLizard.Integration1";
pub const SUMMARY_LIMIT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadFolder {
    pub id: i64,
    pub name: String,
    pub unread: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationState {
    pub protocol_version: u16,
    pub total_unread: i64,
    pub folders: Vec<UnreadFolder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationAction {
    OpenFeedLizard,
    OpenUnread,
    Refresh,
}

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error("integration storage failed: {0}")]
    Storage(String),
    #[error("integration service failed: {0}")]
    Service(String),
}

pub fn read_state(path: &Path) -> Result<IntegrationState, IntegrationError> {
    let library = Library::open_read_only(path)
        .map_err(|error| IntegrationError::Storage(error.to_string()))?;
    let total_unread = library
        .unread_count(ArticleScope::Unread)
        .map_err(|error| IntegrationError::Storage(error.to_string()))?;
    let folders = library
        .unread_folder_summary(SUMMARY_LIMIT)
        .map_err(|error| IntegrationError::Storage(error.to_string()))?
        .into_iter()
        .map(|folder| UnreadFolder {
            id: folder.folder_id,
            name: folder.folder_name,
            unread: folder.unread,
        })
        .collect();
    Ok(IntegrationState {
        protocol_version: 1,
        total_unread,
        folders,
    })
}

pub fn state_json(path: &Path) -> Result<String, IntegrationError> {
    serde_json::to_string(&read_state(path)?)
        .map_err(|error| IntegrationError::Service(error.to_string()))
}

struct IntegrationInterface {
    database_path: PathBuf,
    actions: Sender<IntegrationAction>,
}

#[zbus::interface(name = "io.github.feedlizard.FeedLizard.Integration1")]
impl IntegrationInterface {
    fn get_unread_state(&self) -> fdo::Result<String> {
        state_json(&self.database_path).map_err(|error| fdo::Error::Failed(error.to_string()))
    }

    fn open_feed_lizard(&self) -> fdo::Result<()> {
        self.send(IntegrationAction::OpenFeedLizard)
    }

    fn open_unread(&self) -> fdo::Result<()> {
        self.send(IntegrationAction::OpenUnread)
    }

    fn refresh(&self) -> fdo::Result<()> {
        self.send(IntegrationAction::Refresh)
    }

    #[zbus(signal)]
    async fn unread_changed(emitter: &SignalEmitter<'_>, state: &str) -> zbus::Result<()>;
}

impl IntegrationInterface {
    fn send(&self, action: IntegrationAction) -> fdo::Result<()> {
        self.actions
            .send(action)
            .map_err(|_| fdo::Error::Failed("FeedLizard UI is unavailable".into()))
    }
}

#[derive(Clone)]
pub struct IntegrationHandle {
    notifications: mpsc::Sender<()>,
}

impl IntegrationHandle {
    pub fn notify_unread_changed(&self) {
        let _ = self.notifications.try_send(());
    }
}

pub fn start_service(
    database_path: PathBuf,
    actions: Sender<IntegrationAction>,
) -> Result<IntegrationHandle, IntegrationError> {
    let (notifications, mut notification_receiver) = mpsc::channel::<()>(1);
    let thread_path = database_path.clone();
    thread::Builder::new()
        .name("feedlizard-integration".into())
        .spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("integration runtime starts");
            runtime.block_on(async move {
                let interface = IntegrationInterface {
                    database_path: thread_path.clone(),
                    actions,
                };
                let connection = match Connection::session().await {
                    Ok(connection) => connection,
                    Err(error) => {
                        eprintln!("FeedLizard integration unavailable: {error}");
                        return;
                    }
                };
                if let Err(error) = connection.request_name(SERVICE_NAME).await {
                    eprintln!("FeedLizard integration name unavailable: {error}");
                    return;
                }
                if let Err(error) = connection.object_server().at(OBJECT_PATH, interface).await {
                    eprintln!("FeedLizard integration object unavailable: {error}");
                    return;
                }
                let emitter = match SignalEmitter::new(&connection, OBJECT_PATH) {
                    Ok(emitter) => emitter,
                    Err(error) => {
                        eprintln!("FeedLizard integration signal unavailable: {error}");
                        return;
                    }
                };
                while notification_receiver.recv().await.is_some() {
                    if let Ok(state) = state_json(&thread_path) {
                        let _ = IntegrationInterface::unread_changed(&emitter, &state).await;
                    }
                }
            });
        })
        .map_err(|error| IntegrationError::Service(error.to_string()))?;
    Ok(IntegrationHandle { notifications })
}

#[cfg(test)]
mod tests {
    use super::*;
    use feedlizard_core::parser::FeedFormat;

    #[test]
    fn state_is_bounded_and_contains_no_private_data() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("library.sqlite3");
        let mut library = Library::open(&path).unwrap();
        for index in 0..8 {
            let folder = library
                .create_folder(&format!("Folder {index}"), None, index)
                .unwrap();
            let url = format!("https://private-{index}.example/feed");
            let id = library
                .add_subscription(&url, "Private", FeedFormat::Rss, None, index)
                .unwrap();
            library.move_feed(&id, Some(folder.id), index).unwrap();
            let items = (0..=index)
                .map(|article| {
                    format!(
                        "<item><title>Article {article}</title><guid>{index}-{article}</guid></item>"
                    )
                })
                .collect::<String>();
            let document = format!(
                "<rss version=\"2.0\"><channel><title>Private</title><link>https://private-{index}.example</link>{items}</channel></rss>"
            );
            library.ingest_document(&url, &document, 100).unwrap();
        }
        drop(library);

        let state = read_state(&path).unwrap();
        assert_eq!(state.total_unread, 36);
        assert_eq!(state.folders.len(), SUMMARY_LIMIT);
        assert_eq!(state.folders[0].name, "Folder 7");
        let json = state_json(&path).unwrap();
        assert!(!json.contains("private-"));
        assert!(!json.contains("sqlite"));
        assert!(!json.contains("nsec"));
    }

    #[test]
    fn empty_library_reports_zero() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("library.sqlite3");
        Library::open(&path).unwrap();
        assert_eq!(read_state(&path).unwrap().total_unread, 0);
    }

    #[test]
    fn large_counts_serialize_without_truncation() {
        let state = IntegrationState {
            protocol_version: 1,
            total_unread: 1_000_000,
            folders: vec![],
        };
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: IntegrationState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.total_unread, 1_000_000);
    }
}
