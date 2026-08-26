use feedlizard_storage::{
    ArticleListItem, ArticleScope, FeedRecord, FolderRecord, FullArticle, Library, LibraryStats,
    PageCursor,
};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedScope {
    Library,
    Unread,
    Starred,
    Feed(String),
    Folder(i64),
}

impl OwnedScope {
    fn borrowed(&self) -> ArticleScope<'_> {
        match self {
            Self::Library => ArticleScope::Library,
            Self::Unread => ArticleScope::Unread,
            Self::Starred => ArticleScope::Starred,
            Self::Feed(id) => ArticleScope::Feed(id),
            Self::Folder(id) => ArticleScope::Folder(*id),
        }
    }
}

#[derive(Debug)]
pub enum Command {
    LoadNavigation,
    LoadArticles(OwnedScope),
    LoadMore(OwnedScope, PageCursor),
    Search(String),
    OpenArticle(String),
    SetRead { id: String, read: bool },
    SetStarred { id: String, starred: bool },
    MarkAllRead(OwnedScope),
    ImportOpml(PathBuf),
    ExportOpml(PathBuf),
    CreateFolder(String),
    RenameFolder { id: i64, name: String },
    DeleteFolder(i64),
    RenameFeed { id: String, name: Option<String> },
    RemoveFeed(String),
    MoveFeed { id: String, folder_id: Option<i64> },
}

#[derive(Debug)]
pub enum Event {
    Navigation {
        feeds: Vec<FeedRecord>,
        folders: Vec<FolderRecord>,
        stats: LibraryStats,
    },
    Articles {
        scope: OwnedScope,
        items: Vec<ArticleListItem>,
        next: Option<PageCursor>,
        append: bool,
    },
    SearchResults {
        query: String,
        items: Vec<ArticleListItem>,
    },
    Article(Box<FullArticle>),
    MutationComplete,
    Notice(String),
    Error(String),
}

#[derive(Clone)]
pub struct Worker {
    sender: Sender<Command>,
}

impl Worker {
    pub fn start(database_path: PathBuf) -> (Self, Receiver<Event>) {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("feedlizard-storage".into())
            .spawn(move || run(database_path, command_receiver, event_sender))
            .expect("storage worker thread starts");
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
    let mut library = match Library::open(database_path) {
        Ok(library) => library,
        Err(error) => {
            let _ = events.send(Event::Error(error.to_string()));
            return;
        }
    };
    while let Ok(command) = commands.recv() {
        let result = handle(&mut library, command);
        match result {
            Ok(event) => {
                let _ = events.send(event);
            }
            Err(error) => {
                let _ = events.send(Event::Error(error));
            }
        }
    }
}

fn handle(library: &mut Library, command: Command) -> Result<Event, String> {
    match command {
        Command::LoadNavigation => Ok(Event::Navigation {
            feeds: library.list_feeds().map_err(|error| error.to_string())?,
            folders: library.list_folders().map_err(|error| error.to_string())?,
            stats: library.stats().map_err(|error| error.to_string())?,
        }),
        Command::LoadArticles(scope) => {
            let page = library
                .article_page(scope.borrowed(), 100, None)
                .map_err(|error| error.to_string())?;
            Ok(Event::Articles {
                scope,
                items: page.items,
                next: page.next,
                append: false,
            })
        }
        Command::LoadMore(scope, cursor) => {
            let page = library
                .article_page(scope.borrowed(), 100, Some(&cursor))
                .map_err(|error| error.to_string())?;
            Ok(Event::Articles {
                scope,
                items: page.items,
                next: page.next,
                append: true,
            })
        }
        Command::Search(query) => {
            let items = library
                .search(&query, 100)
                .map_err(|error| error.to_string())?;
            Ok(Event::SearchResults { query, items })
        }
        Command::OpenArticle(id) => library
            .full_article(&id)
            .map(Box::new)
            .map(Event::Article)
            .map_err(|error| error.to_string()),
        Command::SetRead { id, read } => {
            library
                .mark_article_read(&id, read, unix_now())
                .map_err(|error| error.to_string())?;
            Ok(Event::MutationComplete)
        }
        Command::SetStarred { id, starred } => {
            library
                .set_article_starred(&id, starred, unix_now())
                .map_err(|error| error.to_string())?;
            Ok(Event::MutationComplete)
        }
        Command::MarkAllRead(scope) => {
            library
                .mark_all_read(scope.borrowed(), unix_now())
                .map_err(|error| error.to_string())?;
            Ok(Event::MutationComplete)
        }
        Command::ImportOpml(path) => {
            let input = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
            let stats = library
                .import_opml(&input, unix_now())
                .map_err(|error| error.to_string())?;
            Ok(Event::Notice(format!(
                "Imported {} feed{} and {} folder{}",
                stats.feeds_added,
                if stats.feeds_added == 1 { "" } else { "s" },
                stats.folders_created,
                if stats.folders_created == 1 { "" } else { "s" }
            )))
        }
        Command::ExportOpml(path) => {
            let output = library
                .export_opml("Thu, 01 Jan 1970 00:00:00 +0000")
                .map_err(|error| error.to_string())?;
            std::fs::write(path, output).map_err(|error| error.to_string())?;
            Ok(Event::Notice("OPML export complete".into()))
        }
        Command::CreateFolder(name) => {
            library
                .create_folder(&name, None, unix_now())
                .map_err(|error| error.to_string())?;
            Ok(Event::Notice("Folder created".into()))
        }
        Command::RenameFolder { id, name } => {
            library
                .rename_folder(id, &name, unix_now())
                .map_err(|error| error.to_string())?;
            Ok(Event::Notice("Folder renamed".into()))
        }
        Command::DeleteFolder(id) => {
            library
                .delete_folder(id)
                .map_err(|error| error.to_string())?;
            Ok(Event::Notice(
                "Folder removed; its feeds were preserved".into(),
            ))
        }
        Command::RenameFeed { id, name } => {
            library
                .set_feed_custom_name(&id, name.as_deref(), unix_now())
                .map_err(|error| error.to_string())?;
            Ok(Event::Notice("Feed name updated".into()))
        }
        Command::RemoveFeed(id) => {
            library
                .remove_subscription(&id)
                .map_err(|error| error.to_string())?;
            Ok(Event::Notice("Feed removed".into()))
        }
        Command::MoveFeed { id, folder_id } => {
            library
                .move_feed(&id, folder_id, unix_now())
                .map_err(|error| error.to_string())?;
            Ok(Event::Notice("Feed moved".into()))
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
