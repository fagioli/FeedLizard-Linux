use crate::{
    discover_feeds,
    image_worker::{Event as ImageEvent, ImageWorker},
    network_worker::{Command as NetworkCommand, Event as NetworkEvent, NetworkWorker},
    nostr_worker::{Command as NostrCommand, Event as NostrEvent, NostrWorker, SnapshotSummary},
    omarchy,
    worker::{Command, Event, OwnedScope, Worker},
};
use adw::prelude::*;
use chrono::{Datelike, Timelike};
use feedlizard_image::{Fit, Request as ImageRequest};
use feedlizard_integration::{
    IntegrationAction, IntegrationHandle, start_service as start_integration_service,
};
use feedlizard_reader::{Block, Document, Page, PageChunk, PageStyle};
use feedlizard_storage::{ArticleListItem, FeedRecord, FolderRecord, FullArticle, PageCursor};
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::mpsc::Receiver,
};

pub fn install_actions(application: &adw::Application) {
    application.add_action_entries([
        gio::ActionEntry::builder("quit")
            .activate(|app: &adw::Application, _, _| app.quit())
            .build(),
        gio::ActionEntry::builder("refresh-all")
            .activate(|_, _, _| {})
            .build(),
        gio::ActionEntry::builder("search")
            .activate(|_, _, _| {})
            .build(),
    ]);
    application.set_accels_for_action("app.quit", &["<primary>q"]);
    application.set_accels_for_action("app.refresh-all", &["<primary>r", "r"]);
    application.set_accels_for_action("app.search", &["<primary>f", "slash"]);
}

struct View {
    window: adw::ApplicationWindow,
    outer: adw::NavigationSplitView,
    inner: adw::NavigationSplitView,
    sidebar_list: gtk::ListBox,
    feed_list: gtk::ListBox,
    article_list: gtk::ListBox,
    article_title: adw::WindowTitle,
    empty: adw::StatusPage,
    article_scroller: gtk::ScrolledWindow,
    reader_scroller: gtk::ScrolledWindow,
    reader_title: gtk::Label,
    reader_meta: gtk::Label,
    reader_content: gtk::Box,
    reader_mode_stack: gtk::Stack,
    pages_deck: gtk::Stack,
    pages_indicator: gtk::Label,
    pages_previous: gtk::Button,
    pages_next: gtk::Button,
    scroll_mode: gtk::ToggleButton,
    pages_mode: gtk::ToggleButton,
    book_view: gtk::Button,
    reader_star: gtk::ToggleButton,
    open_original: gtk::Button,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    mark_all: gtk::Button,
    refresh_all: gtk::Button,
    add_feed: gtk::Button,
    create_folder: gtk::Button,
    manage: gtk::Button,
    toast: adw::ToastOverlay,
    worker: Worker,
    network: NetworkWorker,
    nostr: NostrWorker,
    images: ImageWorker,
    icons: ImageWorker,
    integration: Option<IntegrationHandle>,
    scope: RefCell<OwnedScope>,
    unread_snapshot_dirty: Cell<bool>,
    article_ids: RefCell<Vec<String>>,
    article_items: RefCell<Vec<ArticleListItem>>,
    article_row_states: RefCell<HashMap<String, ArticleRowState>>,
    image_enrichment_requested: RefCell<HashSet<String>>,
    favicon_discovery_requested: RefCell<HashSet<String>>,
    article_extraction_requested: RefCell<HashSet<String>>,
    extracted_articles: RefCell<HashMap<String, Document>>,
    open_article: RefCell<Option<FullArticle>>,
    feeds: RefCell<Vec<FeedRecord>>,
    folders: RefCell<Vec<FolderRecord>>,
    image_targets: RefCell<HashMap<ImageRequest, Vec<gtk::Picture>>>,
    reader_image_targets: RefCell<HashMap<ImageRequest, Vec<gtk::Picture>>>,
    icon_targets: RefCell<HashMap<ImageRequest, Vec<gtk::Picture>>>,
    icon_textures: RefCell<HashMap<ImageRequest, gtk::gdk::Paintable>>,
    page_count: Cell<usize>,
    page_index: Cell<usize>,
    pages: RefCell<Vec<Page>>,
    page_article_url: RefCell<Option<String>>,
    book_session: RefCell<Option<Rc<BookSession>>>,
    reader_text_size: Cell<f64>,
    next_cursor: RefCell<Option<PageCursor>>,
}

#[derive(Clone)]
struct ArticleRowState {
    container: gtk::Box,
    title: gtk::Label,
    unread: gtk::Image,
    starred: gtk::Image,
    is_unread: Rc<Cell<bool>>,
    is_starred: Rc<Cell<bool>>,
    thumbnail: Option<gtk::Picture>,
}

pub fn build_window(application: &adw::Application) {
    if let Some(window) = application.active_window() {
        window.present();
        return;
    }
    install_css();
    apply_appearance(load_appearance());
    let path = database_path();
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        let dialog = adw::AlertDialog::builder()
            .heading("FeedLizard could not open its library")
            .body(format!(
                "The application data directory could not be created: {error}"
            ))
            .build();
        dialog.add_response("quit", "Quit");
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak]
                application,
                move |_, _| {
                    application.quit();
                }
            ),
        );
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("FeedLizard")
            .default_width(520)
            .default_height(320)
            .build();
        window.present();
        dialog.present(Some(&window));
        return;
    }
    let (worker, events) = Worker::start(path.clone());
    let (network, network_events) = NetworkWorker::start(path.clone());
    let (nostr, nostr_events) = NostrWorker::start(path);
    let image_cache = image_cache_path();
    let (images, image_events) = ImageWorker::start(image_cache.clone());
    let (icons, icon_events) = ImageWorker::start_with_options(
        image_cache.join("feed-icons"),
        12,
        std::time::Duration::from_secs(6),
    );
    let (integration_action_sender, integration_actions) = std::sync::mpsc::channel();
    let integration = start_integration_service(database_path(), integration_action_sender)
        .map_err(|error| eprintln!("FeedLizard desktop integration unavailable: {error}"))
        .ok();
    let view = Rc::new(build_view(
        application,
        worker,
        network,
        nostr,
        images,
        icons,
        integration,
    ));
    connect_view(&view);
    poll_events(&view, events);
    poll_network_events(&view, network_events);
    poll_nostr_events(&view, nostr_events);
    poll_image_events(&view, image_events);
    poll_icon_events(&view, icon_events);
    poll_integration_actions(&view, integration_actions);
    view.worker.send(Command::LoadNavigation);
    view.worker.send(Command::LoadArticles(OwnedScope::Unread));
    view.window.present();
}

fn build_view(
    application: &adw::Application,
    worker: Worker,
    network: NetworkWorker,
    nostr: NostrWorker,
    images: ImageWorker,
    icons: ImageWorker,
    integration: Option<IntegrationHandle>,
) -> View {
    let sidebar_list = gtk::ListBox::new();
    sidebar_list.set_selection_mode(gtk::SelectionMode::Single);
    sidebar_list.add_css_class("navigation-sidebar");
    for (icon, title, tag) in [
        ("mail-unread-symbolic", "Unread", "scope:unread"),
        ("view-list-symbolic", "Library", "scope:library"),
        ("starred-symbolic", "Starred", "scope:starred"),
        ("emblem-system-symbolic", "Settings", "settings"),
    ] {
        sidebar_list.append(&navigation_row(
            icon,
            title,
            tag,
            matches!(tag, "scope:unread" | "scope:starred").then_some(0),
        ));
    }
    let feed_list = gtk::ListBox::new();
    feed_list.set_selection_mode(gtk::SelectionMode::Single);
    feed_list.add_css_class("navigation-sidebar");

    let sidebar_toolbar = adw::ToolbarView::new();
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_title_widget(Some(
        &gtk::Label::builder()
            .label("FeedLizard")
            .css_classes(["title"])
            .build(),
    ));
    let add_feed = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add Feed")
        .build();
    sidebar_header.pack_end(&add_feed);
    let create_folder = gtk::Button::builder()
        .icon_name("folder-new-symbolic")
        .tooltip_text("New Folder")
        .build();
    sidebar_header.pack_end(&create_folder);
    sidebar_toolbar.add_top_bar(&sidebar_header);
    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&feed_list)
        .build();
    install_smooth_wheel_scroll(&sidebar_scroll);
    let sidebar_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_content.append(&sidebar_list);
    sidebar_content.append(
        &gtk::Label::builder()
            .label("Feeds")
            .xalign(0.0)
            .css_classes(["section-heading"])
            .build(),
    );
    sidebar_content.append(&sidebar_scroll);
    sidebar_toolbar.set_content(Some(&sidebar_content));

    let article_title = adw::WindowTitle::new("Unread", "All feeds");
    let article_header = adw::HeaderBar::new();
    article_header.set_title_widget(Some(&article_title));
    let refresh_all = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh All")
        .build();
    article_header.pack_end(&refresh_all);
    article_header.pack_end(
        &gtk::Button::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Search")
            .action_name("app.search")
            .build(),
    );
    let mark_all = gtk::Button::builder()
        .icon_name("mail-mark-read-symbolic")
        .tooltip_text("Mark All as Read")
        .build();
    article_header.pack_end(&mark_all);
    let manage = gtk::Button::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Manage Feed or Folder")
        .visible(false)
        .build();
    article_header.pack_start(&manage);

    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text("Search your library")
        .hexpand(true)
        .build();
    let search_bar = gtk::SearchBar::new();
    search_bar.set_child(Some(&search_entry));
    search_bar.connect_entry(&search_entry);

    let article_list = gtk::ListBox::new();
    article_list.set_selection_mode(gtk::SelectionMode::Single);
    article_list.add_css_class("article-list");
    let article_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&article_list)
        .build();
    install_smooth_wheel_scroll(&article_scroller);
    let empty = adw::StatusPage::builder()
        .icon_name("view-list-symbolic")
        .title("Nothing to read yet")
        .description("Add a feed or refresh your subscriptions.")
        .build();
    let article_stack = gtk::Stack::new();
    article_stack.add_named(&article_scroller, Some("list"));
    article_stack.add_named(&empty, Some("empty"));
    article_stack.set_visible_child_name("empty");
    let articles = adw::ToolbarView::new();
    articles.add_top_bar(&article_header);
    articles.add_top_bar(&search_bar);
    articles.set_content(Some(&article_stack));

    let reader_title = gtk::Label::builder()
        .label("Select an article")
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(72)
        .xalign(0.0)
        .css_classes(["reader-title"])
        .build();
    let reader_meta = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(88)
        .css_classes(["reader-meta"])
        .build();
    let reader_content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    reader_content.set_hexpand(true);
    reader_content.add_css_class("reader-content");
    let reader_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
    reader_box.set_hexpand(true);
    reader_box.add_css_class("reader-page");
    reader_box.append(&reader_title);
    reader_box.append(&reader_meta);
    reader_box.append(&reader_content);
    let reader_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&reader_box)
        .build();
    install_smooth_wheel_scroll(&reader_scroll);
    let pages_deck = gtk::Stack::new();
    let animations = gtk::Settings::default()
        .map(|settings| settings.is_gtk_enable_animations())
        .unwrap_or(true);
    pages_deck.set_transition_type(if animations {
        gtk::StackTransitionType::SlideLeftRight
    } else {
        gtk::StackTransitionType::None
    });
    pages_deck.set_transition_duration(220);
    pages_deck.set_vexpand(true);
    let pages_previous = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Previous Page")
        .build();
    let pages_next = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text("Next Page")
        .build();
    let pages_indicator = gtk::Label::builder()
        .label("Page 1 of 1")
        .css_classes(["dim-label"])
        .hexpand(true)
        .build();
    let pages_footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    pages_footer.add_css_class("pages-footer");
    pages_footer.append(&pages_previous);
    pages_footer.append(&pages_indicator);
    pages_footer.append(&pages_next);
    let pages_shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    pages_shell.add_css_class("pages-shell");
    pages_shell.append(&pages_deck);
    pages_shell.append(&pages_footer);
    let reader_mode_stack = gtk::Stack::new();
    reader_mode_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    reader_mode_stack.add_named(&reader_scroll, Some("scroll"));
    reader_mode_stack.add_named(&pages_shell, Some("pages"));
    reader_mode_stack.set_visible_child_name("scroll");
    let reader_header = adw::HeaderBar::new();
    let scroll_mode = gtk::ToggleButton::builder()
        .label("Scroll")
        .active(true)
        .build();
    let pages_mode = gtk::ToggleButton::builder().label("Pages").build();
    pages_mode.set_group(Some(&scroll_mode));
    let mode = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    mode.add_css_class("linked");
    mode.append(&scroll_mode);
    mode.append(&pages_mode);
    reader_header.set_title_widget(Some(&mode));
    let stack = reader_mode_stack.clone();
    scroll_mode.connect_toggled(move |button| {
        if button.is_active() {
            stack.set_visible_child_name("scroll");
        }
    });
    let stack = reader_mode_stack.clone();
    pages_mode.connect_toggled(move |button| {
        if button.is_active() {
            stack.set_visible_child_name("pages");
        }
    });
    let reader_star = gtk::ToggleButton::builder()
        .icon_name("non-starred-symbolic")
        .tooltip_text("Star")
        .build();
    let open_original = gtk::Button::builder()
        .icon_name("external-link-symbolic")
        .tooltip_text("Open Original")
        .sensitive(false)
        .build();
    let book_view = gtk::Button::builder()
        .icon_name("view-fullscreen-symbolic")
        .tooltip_text("Enter Book View (F11)")
        .sensitive(false)
        .build();
    reader_header.pack_end(&open_original);
    reader_header.pack_end(&reader_star);
    reader_header.pack_end(&book_view);
    let reader = adw::ToolbarView::new();
    reader.add_top_bar(&reader_header);
    reader.set_content(Some(&reader_mode_stack));

    let inner = adw::NavigationSplitView::new();
    // Keep article selection from moving the reader boundary. Wrapped reader
    // content has article-dependent natural widths, so allowing this split to
    // negotiate a width range caused the center pane to grow or shrink as the
    // selected article changed. The adaptive breakpoint still collapses the
    // split for narrow windows, while the reader absorbs deliberate window
    // size changes on wider layouts.
    inner.set_min_sidebar_width(450.0);
    inner.set_max_sidebar_width(450.0);
    inner.set_sidebar(Some(&adw::NavigationPage::new(&articles, "Articles")));
    inner.set_content(Some(&adw::NavigationPage::new(&reader, "Reader")));
    let outer = adw::NavigationSplitView::new();
    outer.set_min_sidebar_width(220.0);
    outer.set_max_sidebar_width(310.0);
    outer.set_sidebar_width_fraction(0.22);
    outer.set_sidebar(Some(&adw::NavigationPage::new(
        &sidebar_toolbar,
        "FeedLizard",
    )));
    outer.set_content(Some(&adw::NavigationPage::new(&inner, "Library")));
    let toast = adw::ToastOverlay::new();
    toast.set_child(Some(&outer));
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("FeedLizard")
        .default_width(window_dimension("FEEDLIZARD_WINDOW_WIDTH", 1440))
        .default_height(window_dimension("FEEDLIZARD_WINDOW_HEIGHT", 860))
        .content(&toast)
        .build();
    for (width, split) in [(720, outer.clone()), (1080, inner.clone())] {
        if let Ok(condition) = adw::BreakpointCondition::parse(&format!("max-width: {width}px")) {
            let breakpoint = adw::Breakpoint::new(condition);
            breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
            window.add_breakpoint(breakpoint);
        }
    }
    let reader_text_size = load_reader_text_size();
    install_reader_text_size(reader_text_size);
    View {
        window,
        outer,
        inner,
        sidebar_list,
        feed_list,
        article_list,
        article_title,
        empty,
        article_scroller,
        reader_scroller: reader_scroll,
        reader_title,
        reader_meta,
        reader_content,
        reader_mode_stack,
        pages_deck,
        pages_indicator,
        pages_previous,
        pages_next,
        scroll_mode,
        pages_mode,
        book_view,
        reader_star,
        open_original,
        search_bar,
        search_entry,
        mark_all,
        refresh_all,
        add_feed,
        create_folder,
        manage,
        toast,
        worker,
        network,
        nostr,
        images,
        icons,
        integration,
        scope: RefCell::new(OwnedScope::Unread),
        unread_snapshot_dirty: Cell::new(false),
        article_ids: RefCell::new(Vec::new()),
        article_items: RefCell::new(Vec::new()),
        article_row_states: RefCell::new(HashMap::new()),
        image_enrichment_requested: RefCell::new(HashSet::new()),
        favicon_discovery_requested: RefCell::new(HashSet::new()),
        article_extraction_requested: RefCell::new(HashSet::new()),
        extracted_articles: RefCell::new(HashMap::new()),
        open_article: RefCell::new(None),
        feeds: RefCell::new(Vec::new()),
        folders: RefCell::new(Vec::new()),
        image_targets: RefCell::new(HashMap::new()),
        reader_image_targets: RefCell::new(HashMap::new()),
        icon_targets: RefCell::new(HashMap::new()),
        icon_textures: RefCell::new(HashMap::new()),
        page_count: Cell::new(0),
        page_index: Cell::new(0),
        pages: RefCell::new(Vec::new()),
        page_article_url: RefCell::new(None),
        book_session: RefCell::new(None),
        reader_text_size: Cell::new(reader_text_size),
        next_cursor: RefCell::new(None),
    }
}

fn connect_view(view: &Rc<View>) {
    let weak = Rc::downgrade(view);
    view.pages_mode.connect_toggled(move |button| {
        if button.is_active()
            && let Some(view) = weak.upgrade()
        {
            request_full_article(&view);
        }
    });
    let weak = Rc::downgrade(view);
    view.sidebar_list.connect_row_activated(move |_, row| {
        let Some(view) = weak.upgrade() else { return };
        view.feed_list.unselect_all();
        activate_sidebar_row(&view, row);
    });
    if let Some(starred_row) = view.sidebar_list.row_at_index(2) {
        install_starred_context_menu(view, &starred_row);
    }
    let weak = Rc::downgrade(view);
    view.feed_list.connect_row_activated(move |_, row| {
        let Some(view) = weak.upgrade() else { return };
        view.sidebar_list.unselect_all();
        activate_sidebar_row(&view, row);
    });
    let weak = Rc::downgrade(view);
    view.article_list.connect_row_activated(move |_, row| {
        let Some(view) = weak.upgrade() else { return };
        if let Some(id) = view.article_ids.borrow().get(row.index() as usize).cloned() {
            if id.is_empty() {
                if let Some(cursor) = view.next_cursor.borrow_mut().take() {
                    view.worker
                        .send(Command::LoadMore(view.scope.borrow().clone(), cursor));
                }
                return;
            }
            view.worker.send(Command::OpenArticle(id.clone()));
            view.worker.send(Command::SetRead { id, read: true });
            view.inner.set_show_content(true);
        }
    });
    let weak = Rc::downgrade(view);
    view.search_entry.connect_search_changed(move |entry| {
        let Some(view) = weak.upgrade() else { return };
        let query = entry.text().trim().to_owned();
        if query.is_empty() {
            view.worker
                .send(Command::LoadArticles(view.scope.borrow().clone()));
        } else {
            view.worker.send(Command::Search(query));
        }
    });
    let weak = Rc::downgrade(view);
    view.reader_star.connect_toggled(move |button| {
        let Some(view) = weak.upgrade() else { return };
        if let Some(article) = view.open_article.borrow().as_ref() {
            view.worker.send(Command::SetStarred {
                id: article.stable_id.clone(),
                starred: button.is_active(),
            });
        }
    });
    let weak = Rc::downgrade(view);
    view.open_original.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            open_article_original(&view);
        }
    });
    let headline_click = gtk::GestureClick::new();
    headline_click.set_button(gtk::gdk::BUTTON_PRIMARY);
    let weak = Rc::downgrade(view);
    headline_click.connect_released(move |_, _, _, _| {
        if let Some(view) = weak.upgrade() {
            open_article_original(&view);
        }
    });
    view.reader_title.add_controller(headline_click);
    let weak = Rc::downgrade(view);
    view.mark_all.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            view.worker
                .send(Command::MarkAllRead(view.scope.borrow().clone()));
        }
    });
    let weak = Rc::downgrade(view);
    view.refresh_all.connect_clicked(move |button| {
        if let Some(view) = weak.upgrade() {
            button.set_sensitive(false);
            view.network.send(NetworkCommand::RefreshAll);
            view.toast.add_toast(adw::Toast::new("Refreshing feeds…"));
        }
    });
    let weak = Rc::downgrade(view);
    view.add_feed.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            show_add_feed(&view);
        }
    });
    let weak = Rc::downgrade(view);
    view.create_folder.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            show_text_dialog(&view, "New Folder", "Folder name", "Create", "", {
                let worker = view.worker.clone();
                move |name| worker.send(Command::CreateFolder(name))
            });
        }
    });
    let weak = Rc::downgrade(view);
    view.manage.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            show_manage_dialog(&view);
        }
    });
    let weak = Rc::downgrade(view);
    view.pages_previous.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            show_page(&view, view.page_index.get().saturating_sub(1));
        }
    });
    let weak = Rc::downgrade(view);
    view.pages_next.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            show_page(&view, view.page_index.get().saturating_add(1));
        }
    });
    let weak = Rc::downgrade(view);
    view.book_view.connect_clicked(move |_| {
        if let Some(view) = weak.upgrade() {
            enter_book_view(&view);
        }
    });
    let gesture = gtk::GestureClick::new();
    let weak = Rc::downgrade(view);
    gesture.connect_released(move |gesture, _, x, _| {
        let Some(view) = weak.upgrade() else { return };
        let width = gesture.widget().map(|widget| widget.width()).unwrap_or(0);
        if x < f64::from(width) * 0.28 {
            show_page(&view, view.page_index.get().saturating_sub(1));
        } else if x > f64::from(width) * 0.72 {
            show_page(&view, view.page_index.get().saturating_add(1));
        }
    });
    view.pages_deck.add_controller(gesture);
    let weak = Rc::downgrade(view);
    view.window
        .application()
        .expect("application")
        .lookup_action("search")
        .and_then(|a| a.downcast::<gio::SimpleAction>().ok())
        .map(|action| {
            action.connect_activate(move |_, _| {
                if let Some(view) = weak.upgrade() {
                    view.search_bar.set_search_mode(true);
                    view.search_entry.grab_focus();
                }
            })
        });
    let weak = Rc::downgrade(view);
    view.window
        .application()
        .expect("application")
        .lookup_action("refresh-all")
        .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        .map(|action| {
            action.connect_activate(move |_, _| {
                if let Some(view) = weak.upgrade()
                    && view.refresh_all.is_sensitive()
                {
                    view.refresh_all.set_sensitive(false);
                    view.network.send(NetworkCommand::RefreshAll);
                    view.toast.add_toast(adw::Toast::new("Refreshing feeds…"));
                }
            })
        });
    install_window_actions(view);
}

fn activate_sidebar_row(view: &Rc<View>, row: &gtk::ListBoxRow) {
    let Some(tag) = row.tooltip_text() else {
        return;
    };
    if tag == "settings" {
        show_settings(view);
        return;
    }
    let scope = match tag.as_str() {
        "scope:unread" => OwnedScope::Unread,
        "scope:library" => OwnedScope::Library,
        "scope:starred" => OwnedScope::Starred,
        value if value.starts_with("feed:") => OwnedScope::Feed(value[5..].to_owned()),
        value if value.starts_with("folder:") => value[7..]
            .parse()
            .ok()
            .map(OwnedScope::Folder)
            .unwrap_or(OwnedScope::Library),
        _ => return,
    };
    view.article_title.set_title(scope_title(&scope));
    let was_unread = matches!(*view.scope.borrow(), OwnedScope::Unread);
    let entering_unread = !was_unread && matches!(scope, OwnedScope::Unread);
    let leaving_unread = was_unread && !matches!(scope, OwnedScope::Unread);
    if entering_unread || leaving_unread {
        view.unread_snapshot_dirty.set(false);
    }
    *view.scope.borrow_mut() = scope.clone();
    view.manage
        .set_visible(matches!(scope, OwnedScope::Feed(_) | OwnedScope::Folder(_)));
    view.worker.send(Command::LoadArticles(scope));
    if leaving_unread {
        view.worker.send(Command::LoadNavigation);
    }
    view.outer.set_show_content(true);
}

fn show_text_dialog(
    view: &Rc<View>,
    title: &str,
    placeholder: &str,
    confirm_label: &str,
    initial: &str,
    confirm: impl Fn(String) + 'static,
) {
    let entry = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .text(initial)
        .activates_default(true)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let confirm_button = gtk::Button::builder()
        .label(confirm_label)
        .css_classes(["suggested-action"])
        .build();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    actions.append(&cancel);
    actions.append(&confirm_button);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["title-2"])
            .build(),
    );
    content.append(&entry);
    content.append(&actions);
    let dialog = gtk::Window::builder()
        .title(title)
        .transient_for(&view.window)
        .modal(true)
        .resizable(false)
        .default_width(440)
        .child(&content)
        .build();
    dialog.set_default_widget(Some(&confirm_button));
    let closing = dialog.clone();
    cancel.connect_clicked(move |_| closing.close());
    let closing = dialog.clone();
    confirm_button.connect_clicked(move |_| {
        let value = entry.text().trim().to_owned();
        if !value.is_empty() {
            confirm(value);
            closing.close();
        }
    });
    dialog.present();
}

fn show_manage_dialog(view: &Rc<View>) {
    match view.scope.borrow().clone() {
        OwnedScope::Feed(id) => show_manage_feed(view, &id),
        OwnedScope::Folder(id) => show_manage_folder(view, id),
        _ => {}
    }
}

fn show_manage_feed(view: &Rc<View>, id: &str) {
    let Some(feed) = view
        .feeds
        .borrow()
        .iter()
        .find(|feed| feed.stable_id == id)
        .cloned()
    else {
        return;
    };
    let name = gtk::Entry::builder()
        .placeholder_text(&feed.publisher_name)
        .text(feed.custom_name.as_deref().unwrap_or(""))
        .build();
    let folders = view.folders.borrow().clone();
    let mut folder_names = vec!["Unfiled".to_owned()];
    folder_names.extend(folders.iter().map(|item| item.name.clone()));
    let name_refs = folder_names.iter().map(String::as_str).collect::<Vec<_>>();
    let folder_model = gtk::StringList::new(&name_refs);
    let folder = gtk::DropDown::new(Some(folder_model), None::<gtk::Expression>);
    let selected = feed
        .folder_id
        .and_then(|id| folders.iter().position(|item| item.id == id))
        .map(|index| index as u32 + 1)
        .unwrap_or(0);
    folder.set_selected(selected);
    let save = gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .build();
    let remove = gtk::Button::builder()
        .label("Remove Feed")
        .css_classes(["destructive-action"])
        .build();
    let refresh = gtk::Button::with_label("Refresh Feed");
    let content = management_content(
        "Manage Feed",
        &[name.clone().upcast(), folder.clone().upcast()],
        &[
            remove.clone().upcast(),
            refresh.clone().upcast(),
            save.clone().upcast(),
        ],
    );
    let dialog = gtk::Window::builder()
        .title("Manage Feed")
        .transient_for(&view.window)
        .modal(true)
        .resizable(false)
        .default_width(480)
        .child(&content)
        .build();
    let worker = view.worker.clone();
    let feed_id = id.to_owned();
    let dialog_save = dialog.clone();
    save.connect_clicked(move |_| {
        let custom = name.text().trim().to_owned();
        worker.send(Command::RenameFeed {
            id: feed_id.clone(),
            name: (!custom.is_empty()).then_some(custom),
        });
        let folder_id = folder
            .selected()
            .checked_sub(1)
            .and_then(|index| folders.get(index as usize))
            .map(|item| item.id);
        worker.send(Command::MoveFeed {
            id: feed_id.clone(),
            folder_id,
        });
        dialog_save.close();
    });
    let worker = view.worker.clone();
    let feed_id = id.to_owned();
    let dialog_remove = dialog.clone();
    remove.connect_clicked(move |_| {
        worker.send(Command::RemoveFeed(feed_id.clone()));
        dialog_remove.close();
    });
    let network = view.network.clone();
    let feed_id = id.to_owned();
    let dialog_refresh = dialog.clone();
    refresh.connect_clicked(move |_| {
        network.send(NetworkCommand::RefreshFeed(feed_id.clone()));
        dialog_refresh.close();
    });
    dialog.present();
}

fn show_manage_folder(view: &Rc<View>, id: i64) {
    let Some(folder) = view
        .folders
        .borrow()
        .iter()
        .find(|folder| folder.id == id)
        .cloned()
    else {
        return;
    };
    let name = gtk::Entry::builder().text(&folder.name).build();
    let save = gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .build();
    let remove = gtk::Button::builder()
        .label("Remove Folder")
        .css_classes(["destructive-action"])
        .build();
    let content = management_content(
        "Manage Folder",
        &[name.clone().upcast()],
        &[remove.clone().upcast(), save.clone().upcast()],
    );
    let dialog = gtk::Window::builder()
        .title("Manage Folder")
        .transient_for(&view.window)
        .modal(true)
        .resizable(false)
        .default_width(440)
        .child(&content)
        .build();
    let worker = view.worker.clone();
    let dialog_save = dialog.clone();
    save.connect_clicked(move |_| {
        let value = name.text().trim().to_owned();
        if !value.is_empty() {
            worker.send(Command::RenameFolder { id, name: value });
            dialog_save.close();
        }
    });
    let worker = view.worker.clone();
    let dialog_remove = dialog.clone();
    remove.connect_clicked(move |_| {
        worker.send(Command::DeleteFolder(id));
        dialog_remove.close();
    });
    dialog.present();
}

fn management_content(title: &str, fields: &[gtk::Widget], buttons: &[gtk::Widget]) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["title-2"])
            .build(),
    );
    for field in fields {
        content.append(field);
    }
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    for button in buttons {
        actions.append(button);
    }
    content.append(&actions);
    content
}

fn install_window_actions(view: &Rc<View>) {
    let application = view.window.application().expect("application");
    for (name, accelerators, handler) in [
        ("next-article", &["j"] as &[&str], WindowAction::Next),
        ("previous-article", &["k"], WindowAction::Previous),
        ("open-article", &["Return"], WindowAction::Open),
        ("toggle-read", &["m"], WindowAction::ToggleRead),
        ("toggle-star", &["s"], WindowAction::ToggleStar),
        (
            "previous-page",
            &["Left", "Page_Up"],
            WindowAction::PreviousPage,
        ),
        ("next-page", &["Right", "Page_Down"], WindowAction::NextPage),
        ("book-view", &["F11"], WindowAction::BookView),
        ("back", &["Escape"], WindowAction::Back),
    ] {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(view);
        action.connect_activate(move |_, _| {
            if let Some(view) = weak.upgrade() {
                perform_window_action(&view, handler);
            }
        });
        view.window.add_action(&action);
        application.set_accels_for_action(&format!("win.{name}"), accelerators);
    }
}

#[derive(Clone, Copy)]
enum WindowAction {
    Next,
    Previous,
    Open,
    ToggleRead,
    ToggleStar,
    PreviousPage,
    NextPage,
    BookView,
    Back,
}

fn perform_window_action(view: &Rc<View>, action: WindowAction) {
    match action {
        WindowAction::Next => move_article_selection(view, 1),
        WindowAction::Previous => move_article_selection(view, -1),
        WindowAction::Open => open_selected_article(view),
        WindowAction::ToggleRead => {
            if let Some(article) = view.open_article.borrow().as_ref() {
                view.worker.send(Command::SetRead {
                    id: article.stable_id.clone(),
                    read: !article.is_read,
                });
            }
        }
        WindowAction::ToggleStar => {
            if let Some(article) = view.open_article.borrow().as_ref() {
                view.reader_star.set_active(!article.is_starred);
            }
        }
        WindowAction::PreviousPage => {
            show_page(view, view.page_index.get().saturating_sub(1));
        }
        WindowAction::NextPage => {
            show_page(view, view.page_index.get().saturating_add(1));
        }
        WindowAction::BookView => enter_book_view(view),
        WindowAction::Back => {
            if view.inner.is_collapsed() && view.inner.shows_content() {
                view.inner.set_show_content(false);
            } else if view.outer.is_collapsed() && view.outer.shows_content() {
                view.outer.set_show_content(false);
            } else if view.search_bar.is_search_mode() {
                view.search_bar.set_search_mode(false);
            }
        }
    }
}

fn move_article_selection(view: &View, delta: i32) {
    let count = view.article_ids.borrow().len() as i32;
    if count == 0 {
        return;
    }
    let current = view
        .article_list
        .selected_row()
        .map(|row| row.index())
        .unwrap_or(if delta > 0 { -1 } else { count });
    let next = (current + delta).clamp(0, count - 1);
    if let Some(row) = view.article_list.row_at_index(next) {
        view.article_list.select_row(Some(&row));
        row.grab_focus();
    }
}

fn open_selected_article(view: &View) {
    let Some(row) = view.article_list.selected_row() else {
        return;
    };
    if let Some(id) = view.article_ids.borrow().get(row.index() as usize).cloned() {
        if id.is_empty() {
            if let Some(cursor) = view.next_cursor.borrow_mut().take() {
                view.worker
                    .send(Command::LoadMore(view.scope.borrow().clone(), cursor));
            }
            return;
        }
        view.worker.send(Command::OpenArticle(id.clone()));
        view.worker.send(Command::SetRead { id, read: true });
        view.inner.set_show_content(true);
    }
}

fn show_add_feed(view: &Rc<View>) {
    let entry = gtk::Entry::builder()
        .placeholder_text("https://example.com/feed.xml")
        .input_purpose(gtk::InputPurpose::Url)
        .activates_default(true)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let discover = gtk::Button::with_label("Discover Feeds");
    let add = gtk::Button::builder()
        .label("Add Feed")
        .css_classes(["suggested-action"])
        .build();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    actions.append(&discover);
    actions.append(&cancel);
    actions.append(&add);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(
        &gtk::Label::builder()
            .label("Add a feed")
            .xalign(0.0)
            .css_classes(["title-2"])
            .build(),
    );
    content.append(
        &gtk::Label::builder()
            .label("Enter an RSS, Atom, JSON Feed, or website address.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    content.append(&entry);
    content.append(&actions);
    let dialog = gtk::Window::builder()
        .title("Add Feed")
        .transient_for(&view.window)
        .modal(true)
        .resizable(false)
        .default_width(480)
        .child(&content)
        .build();
    dialog.set_default_widget(Some(&add));
    let dialog_for_cancel = dialog.clone();
    cancel.connect_clicked(move |_| dialog_for_cancel.close());
    let weak = Rc::downgrade(view);
    let dialog_for_discover = dialog.clone();
    discover.connect_clicked(move |_| {
        dialog_for_discover.close();
        if let Some(view) = weak.upgrade() {
            show_discover_feeds(&view);
        }
    });
    let weak = Rc::downgrade(view);
    let dialog_for_add = dialog.clone();
    add.connect_clicked(move |_| {
        let url = entry.text().trim().to_owned();
        if !url.is_empty() {
            if let Some(view) = weak.upgrade() {
                view.network.send(NetworkCommand::AddFeed(url));
                view.toast.add_toast(adw::Toast::new("Adding feed…"));
            }
            dialog_for_add.close();
        }
    });
    dialog.present();
}

fn show_discover_feeds(view: &Rc<View>) {
    let subscribed = discover_feeds::subscribed_ids(&view.feeds.borrow());
    let selections = Rc::new(RefCell::new(
        Vec::<(discover_feeds::Entry, gtk::CheckButton)>::new(),
    ));
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");

    for category in discover_feeds::CATEGORIES {
        let heading = gtk::ListBoxRow::new();
        heading.set_activatable(false);
        heading.set_selectable(false);
        heading.set_child(Some(
            &gtk::Label::builder()
                .label(category)
                .xalign(0.0)
                .css_classes(["heading"])
                .margin_top(14)
                .margin_bottom(6)
                .margin_start(12)
                .margin_end(12)
                .build(),
        ));
        list.append(&heading);
        for entry in discover_feeds::ENTRIES
            .iter()
            .filter(|entry| entry.category == category)
        {
            let already_subscribed = discover_feeds::is_subscribed(entry, &subscribed);
            let check = gtk::CheckButton::builder()
                .label(entry.name)
                .sensitive(!already_subscribed)
                .tooltip_text(entry.website_url)
                .build();
            let status = gtk::Label::builder()
                .label(if already_subscribed { "Subscribed" } else { "" })
                .css_classes(["dim-label"])
                .build();
            let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row_box.set_margin_top(10);
            row_box.set_margin_bottom(10);
            row_box.set_margin_start(12);
            row_box.set_margin_end(12);
            row_box.append(&check);
            status.set_hexpand(true);
            status.set_halign(gtk::Align::End);
            row_box.append(&status);
            let row = gtk::ListBoxRow::new();
            row.set_activatable(!already_subscribed);
            row.set_child(Some(&row_box));
            let check_for_row = check.clone();
            row.connect_activate(move |_| check_for_row.set_active(!check_for_row.is_active()));
            list.append(&row);
            selections.borrow_mut().push((*entry, check));
        }
    }

    let cancel = gtk::Button::with_label("Cancel");
    let add = gtk::Button::builder()
        .label("Add Selected Feeds")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    for (_, check) in selections.borrow().iter() {
        let add = add.clone();
        let selections = Rc::clone(&selections);
        check.connect_toggled(move |_| {
            let count = selections
                .borrow()
                .iter()
                .filter(|(_, check)| check.is_active())
                .count();
            add.set_sensitive(count > 0);
            add.set_label(&format!(
                "Add {count} {}",
                if count == 1 { "Feed" } else { "Feeds" }
            ));
        });
    }
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    actions.append(&cancel);
    actions.append(&add);
    let intro = gtk::Box::new(gtk::Orientation::Vertical, 4);
    intro.append(
        &gtk::Label::builder()
            .label("Start with some great feeds")
            .xalign(0.0)
            .css_classes(["title-2"])
            .build(),
    );
    intro.append(
        &gtk::Label::builder()
            .label("Choose any feeds you’d like to follow. You can add or remove feeds anytime.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.append(&intro);
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list)
        .build();
    content.append(&scroller);
    content.append(&actions);
    let dialog = gtk::Window::builder()
        .title("Discover Feeds")
        .transient_for(&view.window)
        .modal(true)
        .default_width(560)
        .default_height(720)
        .child(&content)
        .build();
    let closing = dialog.clone();
    cancel.connect_clicked(move |_| closing.close());
    let weak = Rc::downgrade(view);
    let closing = dialog.clone();
    add.connect_clicked(move |button| {
        let selected = selections
            .borrow()
            .iter()
            .filter(|(_, check)| check.is_active())
            .map(|(entry, _)| (entry.name.to_owned(), entry.feed_url.to_owned()))
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return;
        }
        button.set_sensitive(false);
        if let Some(view) = weak.upgrade() {
            view.network
                .send(NetworkCommand::AddDiscoveredFeeds(selected));
            view.toast
                .add_toast(adw::Toast::new("Adding selected feeds…"));
        }
        closing.close();
    });
    dialog.present();
}

fn show_discovery_candidates(view: &Rc<View>, candidates: Vec<(String, Option<String>)>) {
    let labels = candidates
        .iter()
        .map(|(url, title)| title.as_deref().unwrap_or(url).to_owned())
        .collect::<Vec<_>>();
    let label_refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let picker = gtk::DropDown::from_strings(&label_refs);
    picker.set_hexpand(true);
    let detail = gtk::Label::builder()
        .label(&candidates[0].0)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .css_classes(["dim-label"])
        .build();
    let candidate_urls = Rc::new(candidates);
    let urls = Rc::clone(&candidate_urls);
    let detail_for_change = detail.clone();
    picker.connect_selected_notify(move |picker| {
        if let Some((url, _)) = urls.get(picker.selected() as usize) {
            detail_for_change.set_text(url);
        }
    });
    let cancel = gtk::Button::with_label("Cancel");
    let add = gtk::Button::builder()
        .label("Add Selected Feed")
        .css_classes(["suggested-action"])
        .build();
    let content = management_content(
        "Choose a feed",
        &[picker.clone().upcast(), detail.upcast()],
        &[cancel.clone().upcast(), add.clone().upcast()],
    );
    let dialog = gtk::Window::builder()
        .title("Choose Feed")
        .transient_for(&view.window)
        .modal(true)
        .resizable(false)
        .default_width(520)
        .child(&content)
        .build();
    let closing = dialog.clone();
    cancel.connect_clicked(move |_| closing.close());
    let network = view.network.clone();
    let closing = dialog.clone();
    add.connect_clicked(move |_| {
        if let Some((url, _)) = candidate_urls.get(picker.selected() as usize) {
            network.send(NetworkCommand::AddFeed(url.clone()));
            closing.close();
        }
    });
    dialog.present();
}

fn show_settings(view: &Rc<View>) {
    let dialog = adw::PreferencesDialog::builder()
        .title("FeedLizard Settings")
        .content_width(620)
        .content_height(640)
        .build();
    let page = adw::PreferencesPage::new();
    let reader = adw::PreferencesGroup::builder().title("Reader").build();
    let text_size = adw::SpinRow::with_range(80.0, 160.0, 5.0);
    text_size.set_title("Text size");
    text_size.set_subtitle("Percentage of the default article text size");
    text_size.set_value(view.reader_text_size.get());
    let weak = Rc::downgrade(view);
    text_size.connect_value_notify(move |row| {
        let Some(view) = weak.upgrade() else { return };
        let value = row.value().clamp(80.0, 160.0);
        view.reader_text_size.set(value);
        install_reader_text_size(value);
        save_reader_text_size(value);
        let open_article = view.open_article.borrow().clone();
        if let Some(article) = open_article {
            show_article(&view, article);
        }
    });
    reader.add(&text_size);
    let appearance = adw::ComboRow::builder()
        .title("Appearance")
        .subtitle("Use the system appearance or choose a Reader theme")
        .model(&gtk::StringList::new(&["System", "Light", "Dark"]))
        .selected(load_appearance().index())
        .build();
    appearance.connect_selected_notify(|row| {
        let appearance = Appearance::from_index(row.selected());
        apply_appearance(appearance);
        save_appearance(appearance);
    });
    reader.add(&appearance);
    page.add(&reader);

    let keyboard = adw::PreferencesGroup::builder()
        .title("Keyboard")
        .description("Shortcuts work when focus is outside a text field.")
        .build();
    keyboard.add(
        &adw::ActionRow::builder()
            .title("Navigate and read")
            .subtitle("J / K  Previous or next · Enter  Open · M  Read/unread · S  Star")
            .build(),
    );
    keyboard.add(
        &adw::ActionRow::builder()
            .title("Search, refresh, and Pages")
            .subtitle("/  Search · R  Refresh · Esc  Back · ← / →  Turn page")
            .build(),
    );
    page.add(&keyboard);

    let library = adw::PreferencesGroup::builder().title("Library").build();
    let retention = adw::ActionRow::builder()
        .title("Article retention")
        .subtitle("Unstarred articles are retained for 30 days. Starred articles are kept.")
        .build();
    library.add(&retention);
    let import = adw::ActionRow::builder()
        .title("Import OPML")
        .subtitle("Import subscriptions and folders from another reader")
        .activatable(true)
        .build();
    import.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let export = adw::ActionRow::builder()
        .title("Export OPML")
        .subtitle("Save a portable copy of your subscriptions")
        .activatable(true)
        .build();
    export.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let weak = Rc::downgrade(view);
    import.connect_activated(move |_| {
        let Some(view) = weak.upgrade() else { return };
        let dialog = gtk::FileDialog::builder()
            .title("Import OPML")
            .accept_label("Import")
            .build();
        let weak = Rc::downgrade(&view);
        dialog.open(
            Some(&view.window),
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                    && let Some(view) = weak.upgrade()
                {
                    view.worker.send(Command::ImportOpml(path));
                }
            },
        );
    });
    let weak = Rc::downgrade(view);
    export.connect_activated(move |_| {
        let Some(view) = weak.upgrade() else { return };
        let dialog = gtk::FileDialog::builder()
            .title("Export OPML")
            .accept_label("Export")
            .initial_name("FeedLizard Subscriptions.opml")
            .build();
        let weak = Rc::downgrade(&view);
        dialog.save(
            Some(&view.window),
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                    && let Some(view) = weak.upgrade()
                {
                    view.worker.send(Command::ExportOpml(path));
                }
            },
        );
    });
    library.add(&import);
    library.add(&export);
    page.add(&library);

    let backup = adw::PreferencesGroup::builder()
        .title("Backup")
        .description(
            "Optional, manual, encrypted subscription backup. Local SQLite remains authoritative.",
        )
        .build();
    let nostr = adw::ActionRow::builder()
        .title("Nostr Backup")
        .subtitle("Encrypted OPML backup on independent relays")
        .activatable(true)
        .build();
    nostr.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let weak = Rc::downgrade(view);
    nostr.connect_activated(move |_| {
        let Some(view) = weak.upgrade() else { return };
        view.nostr.send(NostrCommand::Status);
    });
    backup.add(&nostr);
    page.add(&backup);

    if omarchy::detected() {
        let integrations = adw::PreferencesGroup::builder()
            .title("Integrations")
            .build();
        let omarchy_row = adw::ActionRow::builder()
            .title("FeedLizard for Omarchy")
            .subtitle("Add a compact RSS companion for unread counts and quick actions")
            .activatable(true)
            .build();
        omarchy_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let weak = Rc::downgrade(view);
        omarchy_row.connect_activated(move |_| {
            let Some(view) = weak.upgrade() else { return };
            show_omarchy_integration(&view);
        });
        integrations.add(&omarchy_row);
        page.add(&integrations);
    }

    let about = adw::PreferencesGroup::builder()
        .title("About FeedLizard")
        .build();
    about.add(
        &adw::ActionRow::builder()
            .title("Version")
            .subtitle(format_display_version(env!("CARGO_PKG_VERSION")))
            .build(),
    );
    about.add(
        &adw::ActionRow::builder()
            .title("Private by design")
            .subtitle("Local-first · No account · No telemetry")
            .build(),
    );
    if let Some(address) = option_env!("FEEDLIZARD_SUPPORT_LIGHTNING_ADDRESS") {
        let support = adw::ActionRow::builder()
            .title("Support FeedLizard")
            .subtitle(format!("Bitcoin Lightning · {address}"))
            .build();
        let copy = gtk::Button::builder()
            .label("Copy Address")
            .valign(gtk::Align::Center)
            .build();
        let address = address.to_owned();
        copy.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&address);
            }
        });
        support.add_suffix(&copy);
        about.add(&support);
    }
    page.add(&about);
    dialog.add(&page);
    dialog.present(Some(&view.window));
}

fn format_display_version(version: &str) -> String {
    if let Some((base, beta)) = version.split_once("-beta.") {
        format!("{base} Beta {beta}")
    } else if let Some((base, rc)) = version.split_once("-rc.") {
        format!("{base} RC {rc}")
    } else {
        version.to_owned()
    }
}

fn show_omarchy_integration(view: &Rc<View>) {
    let command = omarchy::install_command();
    let body = if command.is_some() {
        "Copy the supported install command, then run it on the host. Omarchy will show its normal security warning and ask you to approve the plugin. FeedLizard never bypasses that confirmation or writes to your Omarchy configuration directly."
    } else {
        "The compact FeedLizard Omarchy companion is ready, but its standalone plugin repository has not been published yet. Once available, installation will use Omarchy’s normal confirmed plugin workflow."
    };
    let dialog = adw::AlertDialog::builder()
        .heading("FeedLizard for Omarchy")
        .body(body)
        .build();
    dialog.add_response("close", "Close");
    if command.is_some() {
        dialog.add_response("copy", "Copy Install Command");
        dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);
    }
    dialog.connect_response(None, move |_, response| {
        if response == "copy"
            && let Some(command) = &command
            && let Some(display) = gtk::gdk::Display::default()
        {
            display.clipboard().set_text(command);
        }
    });
    dialog.present(Some(&view.window));
}

fn show_nostr_setup(view: &Rc<View>) {
    let dialog = adw::AlertDialog::builder()
        .heading("Set Up Nostr Backup")
        .body("Use an existing Nostr key, or generate a dedicated key for better separation from your public identity. Backup contents are encrypted, but relays can still observe the key, timing, and event size.")
        .build();
    dialog.add_responses(&[
        ("cancel", "Cancel"),
        ("existing", "Use Existing Key"),
        ("generate", "Generate New Key"),
    ]);
    dialog.set_response_appearance("generate", adw::ResponseAppearance::Suggested);
    let weak = Rc::downgrade(view);
    dialog.connect_response(None, move |_, response| {
        let Some(view) = weak.upgrade() else { return };
        match response {
            "existing" => show_existing_key_dialog(&view),
            "generate" => view.nostr.send(NostrCommand::Generate),
            _ => {}
        }
    });
    dialog.present(Some(&view.window));
}

fn show_existing_key_dialog(view: &Rc<View>) {
    let entry = gtk::PasswordEntry::builder()
        .placeholder_text("nsec1…")
        .show_peek_icon(true)
        .hexpand(true)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&gtk::Label::builder()
        .label("The key is validated locally and stored only through Linux Secret Service. It is never sent to a relay. Using a public identity may make backup activity linkable to it.")
        .wrap(true)
        .xalign(0.0)
        .build());
    content.append(&entry);
    let dialog = adw::AlertDialog::builder()
        .heading("Use Existing Key")
        .extra_child(&content)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("save", "Save Securely")]);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("save", false);
    let dialog_for_entry = dialog.clone();
    entry.connect_changed(move |entry| {
        dialog_for_entry.set_response_enabled("save", entry.text().starts_with("nsec1"));
    });
    let weak = Rc::downgrade(view);
    dialog.connect_response(None, move |_, response| {
        if response == "save" {
            let Some(view) = weak.upgrade() else { return };
            view.nostr
                .send(NostrCommand::UseExisting(entry.text().to_string()));
        }
    });
    dialog.present(Some(&view.window));
}

fn show_generated_key(
    view: &Rc<View>,
    nsec: String,
    identity: feedlizard_nostr_backup::KeyIdentity,
) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.append(
        &gtk::Label::builder()
            .label("Save this key somewhere safe.")
            .css_classes(["title-2"])
            .xalign(0.0)
            .build(),
    );
    content.append(&gtk::Label::builder()
        .label("This key is required to restore your Nostr backup on another computer. FeedLizard cannot recover it if you lose it.")
        .wrap(true)
        .xalign(0.0)
        .build());
    let key = gtk::Entry::builder()
        .text(&nsec)
        .editable(false)
        .visibility(false)
        .secondary_icon_name("view-reveal-symbolic")
        .build();
    key.connect_icon_press(|entry, _| {
        let visible = entry.property::<bool>("visibility");
        entry.set_visibility(!visible);
    });
    content.append(&key);
    let copy = gtk::Button::with_label("Copy Key");
    let nsec_for_copy = nsec.clone();
    copy.connect_clicked(move |_| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&nsec_for_copy);
        }
    });
    content.append(&copy);
    content.append(
        &gtk::Label::builder()
            .label(format!("Backup identity: {}", identity.npub))
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .xalign(0.0)
            .build(),
    );
    let saved = gtk::CheckButton::with_label("I have saved this key somewhere safe");
    content.append(&saved);
    let complete = gtk::Button::builder()
        .label("Complete Setup")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    content.append(&complete);
    saved
        .bind_property("active", &complete, "sensitive")
        .sync_create()
        .build();
    let dialog = gtk::Window::builder()
        .title("Save Your Nostr Key")
        .transient_for(&view.window)
        .modal(true)
        .resizable(false)
        .default_width(560)
        .child(&content)
        .build();
    let worker = view.nostr.clone();
    let closing = dialog.clone();
    complete.connect_clicked(move |_| {
        worker.send(NostrCommand::StoreGenerated(nsec.clone()));
        closing.close();
    });
    dialog.present();
}

fn show_nostr_management(view: &Rc<View>, identity: &feedlizard_nostr_backup::KeyIdentity) {
    let dialog = adw::AlertDialog::builder()
        .heading("Nostr Backup")
        .body(format!("Configured as {}\n\nBackups are manual and contain only encrypted subscriptions, folders, and custom names. Relays are independent and may delete data.", identity.npub))
        .build();
    dialog.add_responses(&[
        ("close", "Close"),
        ("remove", "Remove Nostr Key"),
        ("restore", "Restore from Nostr"),
        ("backup", "Back Up Now"),
    ]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_response_appearance("backup", adw::ResponseAppearance::Suggested);
    let weak = Rc::downgrade(view);
    dialog.connect_response(None, move |_, response| {
        let Some(view) = weak.upgrade() else { return };
        match response {
            "backup" => view.nostr.send(NostrCommand::BackUpNow),
            "restore" => view.nostr.send(NostrCommand::FindRestore),
            "remove" => confirm_remove_nostr_key(&view),
            _ => {}
        }
    });
    dialog.present(Some(&view.window));
}

fn confirm_remove_nostr_key(view: &Rc<View>) {
    let dialog = adw::AlertDialog::builder()
        .heading("Remove Nostr Key?")
        .body("This disables backup and restore on this computer. It does not remove encrypted events already held by relays.")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove Key")]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let worker = view.nostr.clone();
    dialog.connect_response(Some("remove"), move |_, _| {
        worker.send(NostrCommand::RemoveKey)
    });
    dialog.present(Some(&view.window));
}

fn show_restore_confirmation(
    view: &Rc<View>,
    created_at: i64,
    subscriptions: usize,
    folders: usize,
    feeds_to_add: usize,
    feeds_already_present: usize,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Restore This Backup?")
        .body(format!("{}\n{subscriptions} subscriptions · {folders} folders\n{feeds_to_add} feeds to add · {feeds_already_present} already present\n\nRestore merges subscriptions, folders, and custom names transactionally. Local-only subscriptions are preserved.", format_snapshot_time(created_at)))
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("restore", "Restore This Backup")]);
    dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
    let worker = view.nostr.clone();
    dialog.connect_response(Some("restore"), move |_, _| {
        worker.send(NostrCommand::ConfirmRestore)
    });
    dialog.present(Some(&view.window));
}

fn show_restore_history(view: &Rc<View>, snapshots: Vec<SnapshotSummary>) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.set_margin_top(12);
    content.set_margin_bottom(18);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let initial = snapshots
        .len()
        .min(feedlizard_nostr_backup::DEFAULT_VISIBLE_SNAPSHOTS);
    for (index, snapshot) in snapshots.iter().take(initial).enumerate() {
        append_snapshot_row(&list, view, snapshot, index == 0);
    }
    content.append(&list);
    let show_older = gtk::Button::builder()
        .label("Show Older Backups")
        .visible(snapshots.len() > initial)
        .build();
    let list_for_older = list.clone();
    let weak = Rc::downgrade(view);
    let older = snapshots.into_iter().skip(initial).collect::<Vec<_>>();
    show_older.connect_clicked(move |button| {
        let Some(view) = weak.upgrade() else { return };
        for snapshot in &older {
            append_snapshot_row(&list_for_older, &view, snapshot, false);
        }
        button.set_visible(false);
    });
    content.append(&show_older);
    content.append(
        &gtk::Label::builder()
            .label(
                "Relays are independent and may retain different subsets of your backup history.",
            )
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(300)
        .max_content_height(620)
        .child(&content)
        .build();
    let dialog = gtk::Window::builder()
        .title("Available Backups")
        .transient_for(&view.window)
        .modal(true)
        .default_width(560)
        .default_height(520)
        .child(&scroller)
        .build();
    dialog.present();
}

fn append_snapshot_row(
    list: &gtk::ListBox,
    view: &Rc<View>,
    snapshot: &SnapshotSummary,
    latest: bool,
) {
    let row = adw::ActionRow::builder()
        .title(format_snapshot_time(snapshot.created_at))
        .subtitle(format!(
            "{} feeds · {} folders",
            snapshot.subscriptions, snapshot.folders
        ))
        .activatable(true)
        .build();
    if latest {
        row.add_suffix(
            &gtk::Label::builder()
                .label("Latest")
                .css_classes(["accent", "caption-heading"])
                .valign(gtk::Align::Center)
                .build(),
        );
    }
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let event_id = snapshot.event_id.clone();
    let worker = view.nostr.clone();
    row.connect_activated(move |row| {
        worker.send(NostrCommand::PreviewRestore(event_id.clone()));
        if let Some(window) = row
            .root()
            .as_ref()
            .and_then(|root| root.downcast_ref::<gtk::Window>())
        {
            window.close();
        }
    });
    list.append(&row);
}

fn format_snapshot_time(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|time| format_snapshot_datetime(time.with_timezone(&chrono::Local)))
        .unwrap_or_else(|| "Unknown backup time".into())
}

fn format_snapshot_datetime<Tz>(time: chrono::DateTime<Tz>) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    time.format("%B %-d, %Y at %-I:%M %p").to_string()
}

fn poll_events(view: &Rc<View>, events: Receiver<Event>) {
    let view = Rc::clone(view);
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if !view.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        while let Ok(event) = events.try_recv() {
            apply_event(&view, event);
        }
        glib::ControlFlow::Continue
    });
}

fn poll_network_events(view: &Rc<View>, events: Receiver<NetworkEvent>) {
    let view = Rc::clone(view);
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if !view.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        while let Ok(event) = events.try_recv() {
            view.refresh_all.set_sensitive(true);
            match event {
                NetworkEvent::FeedAdded { feed_id, articles } => {
                    view.toast.add_toast(adw::Toast::new(&format!(
                        "Feed added with {articles} article{}",
                        if articles == 1 { "" } else { "s" }
                    )));
                    view.worker.send(Command::LoadNavigation);
                    view.worker
                        .send(Command::LoadArticles(OwnedScope::Feed(feed_id)));
                }
                NetworkEvent::DiscoveryCandidates(candidates) => {
                    if candidates.is_empty() {
                        view.toast
                            .add_toast(adw::Toast::new("No feeds were discovered"));
                    } else {
                        show_discovery_candidates(&view, candidates);
                    }
                }
                NetworkEvent::DiscoveredFeedsAdded {
                    added,
                    articles,
                    failures,
                } => {
                    let mut message = format!(
                        "Added {added} {} with {articles} {}",
                        if added == 1 { "feed" } else { "feeds" },
                        if articles == 1 { "article" } else { "articles" }
                    );
                    if !failures.is_empty() {
                        message.push_str(&format!("; couldn’t add {}", failures.join(", ")));
                    }
                    view.toast.add_toast(adw::Toast::new(&message));
                    view.worker.send(Command::LoadNavigation);
                    view.worker
                        .send(Command::LoadArticles(view.scope.borrow().clone()));
                }
                NetworkEvent::FeedRefreshed { inserted, failed } => {
                    let message = if failed {
                        "Feed refresh failed; existing articles were preserved".to_owned()
                    } else if inserted == 0 {
                        "Feed is unchanged".to_owned()
                    } else {
                        format!(
                            "Added {inserted} new article{}",
                            if inserted == 1 { "" } else { "s" }
                        )
                    };
                    view.toast.add_toast(adw::Toast::new(&message));
                    view.worker.send(Command::LoadNavigation);
                    view.worker
                        .send(Command::LoadArticles(view.scope.borrow().clone()));
                }
                NetworkEvent::RefreshComplete(summary) => {
                    view.toast.add_toast(adw::Toast::new(&format!(
                        "Refresh complete: {} updated, {} unchanged, {} failed",
                        summary.successful, summary.unchanged, summary.failed
                    )));
                    view.worker.send(Command::LoadNavigation);
                    view.worker
                        .send(Command::LoadArticles(view.scope.borrow().clone()));
                }
                NetworkEvent::ArticleImagesDiscovered(images) => {
                    for (article_id, image_url) in images {
                        if let Some(item) = view
                            .article_items
                            .borrow_mut()
                            .iter_mut()
                            .find(|item| item.stable_id == article_id)
                        {
                            item.thumbnail_url = Some(image_url.clone());
                        }
                        if let Some(state) = view.article_row_states.borrow().get(&article_id)
                            && let Some(picture) = &state.thumbnail
                        {
                            load_article_thumbnail(&view, picture, &image_url);
                        }
                    }
                }
                NetworkEvent::FaviconsDiscovered(icons) => {
                    if !icons.is_empty() {
                        view.worker.send(Command::LoadNavigation);
                    }
                }
                NetworkEvent::ArticleExtracted {
                    article_id,
                    mut document,
                } => {
                    view.extracted_articles
                        .borrow_mut()
                        .insert(article_id.clone(), document.clone());
                    let article = view.open_article.borrow().clone();
                    if let Some(article) = article.filter(|article| article.stable_id == article_id)
                    {
                        document.blocks.insert(
                            0,
                            Block::Heading {
                                level: 1,
                                text: article.title.clone(),
                            },
                        );
                        render_pages(&view, &document, article.url.as_deref());
                    }
                }
                NetworkEvent::ArticleExtractionFailed { article_id, error } => {
                    view.article_extraction_requested
                        .borrow_mut()
                        .remove(&article_id);
                    if view
                        .open_article
                        .borrow()
                        .as_ref()
                        .is_some_and(|article| article.stable_id == article_id)
                    {
                        view.toast.add_toast(adw::Toast::new(&error));
                    }
                }
                NetworkEvent::Error(error) => view.toast.add_toast(adw::Toast::new(&error)),
            }
        }
        glib::ControlFlow::Continue
    });
}

fn poll_nostr_events(view: &Rc<View>, events: Receiver<NostrEvent>) {
    let view = Rc::clone(view);
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if !view.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        while let Ok(event) = events.try_recv() {
            match event {
                NostrEvent::Status(Some(identity)) => show_nostr_management(&view, &identity),
                NostrEvent::Status(None) => show_nostr_setup(&view),
                NostrEvent::Generated { nsec, identity } => {
                    show_generated_key(&view, nsec, identity)
                }
                NostrEvent::Configured(identity) => view.toast.add_toast(adw::Toast::new(
                    &format!("Nostr backup configured as {}", identity.npub),
                )),
                NostrEvent::BackupComplete { successful, failed } => {
                    view.toast.add_toast(adw::Toast::new(&format!(
                        "Encrypted backup accepted by {successful} relay{}{}",
                        if successful == 1 { "" } else { "s" },
                        if failed > 0 {
                            format!("; {failed} failed")
                        } else {
                            String::new()
                        }
                    )))
                }
                NostrEvent::RestoreHistory(snapshots) => show_restore_history(&view, snapshots),
                NostrEvent::RestorePreview {
                    created_at,
                    subscriptions,
                    folders,
                    feeds_to_add,
                    feeds_already_present,
                } => show_restore_confirmation(
                    &view,
                    created_at,
                    subscriptions,
                    folders,
                    feeds_to_add,
                    feeds_already_present,
                ),
                NostrEvent::RestoreComplete { added, duplicates } => {
                    view.toast.add_toast(adw::Toast::new(&format!(
                        "Restore complete: {added} added, {duplicates} already present"
                    )));
                    view.worker.send(Command::LoadNavigation);
                    view.worker
                        .send(Command::LoadArticles(view.scope.borrow().clone()));
                }
                NostrEvent::KeyRemoved => view
                    .toast
                    .add_toast(adw::Toast::new("Nostr key removed from secure storage")),
                NostrEvent::Error(error) => view
                    .toast
                    .add_toast(adw::Toast::new(&format!("Nostr backup: {error}"))),
            }
        }
        glib::ControlFlow::Continue
    });
}

fn poll_image_events(view: &Rc<View>, events: Receiver<ImageEvent>) {
    let view = Rc::clone(view);
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        if !view.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        while let Ok(event) = events.try_recv() {
            match event {
                ImageEvent::Loaded { request, image } => {
                    let bytes = glib::Bytes::from_owned(image.rgba);
                    let texture = gtk::gdk::MemoryTexture::new(
                        image.width as i32,
                        image.height as i32,
                        gtk::gdk::MemoryFormat::R8g8b8a8,
                        &bytes,
                        (image.width * 4) as usize,
                    );
                    if let Some(targets) = view.image_targets.borrow_mut().remove(&request) {
                        for picture in targets {
                            picture.set_paintable(Some(&texture));
                            picture.set_opacity(1.0);
                            picture.remove_css_class("image-placeholder");
                        }
                    }
                    if let Some(targets) = view.reader_image_targets.borrow_mut().remove(&request) {
                        for picture in targets {
                            picture.set_paintable(Some(&texture));
                            picture.set_opacity(1.0);
                            picture.remove_css_class("image-placeholder");
                        }
                    }
                }
                ImageEvent::Failed { request } => {
                    if let Some(targets) = view.image_targets.borrow_mut().remove(&request) {
                        for picture in targets {
                            picture.set_opacity(0.0);
                        }
                    }
                    if let Some(targets) = view.reader_image_targets.borrow_mut().remove(&request) {
                        for picture in targets {
                            picture.set_visible(false);
                        }
                    }
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

fn poll_icon_events(view: &Rc<View>, events: Receiver<ImageEvent>) {
    let view = Rc::clone(view);
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        if !view.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        while let Ok(event) = events.try_recv() {
            match event {
                ImageEvent::Loaded { request, image } => {
                    let bytes = glib::Bytes::from_owned(image.rgba);
                    let texture: gtk::gdk::Paintable = gtk::gdk::MemoryTexture::new(
                        image.width as i32,
                        image.height as i32,
                        gtk::gdk::MemoryFormat::R8g8b8a8,
                        &bytes,
                        (image.width * 4) as usize,
                    )
                    .upcast();
                    view.icon_textures
                        .borrow_mut()
                        .insert(request.clone(), texture.clone());
                    if let Some(targets) = view.icon_targets.borrow_mut().remove(&request) {
                        for picture in targets {
                            picture.set_paintable(Some(&texture));
                            picture.set_opacity(1.0);
                            picture.remove_css_class("image-placeholder");
                        }
                    }
                }
                ImageEvent::Failed { request } => {
                    view.icon_targets.borrow_mut().remove(&request);
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

fn poll_integration_actions(view: &Rc<View>, actions: Receiver<IntegrationAction>) {
    let view = Rc::clone(view);
    glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        if !view.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        while let Ok(action) = actions.try_recv() {
            match action {
                IntegrationAction::OpenFeedLizard => view.window.present(),
                IntegrationAction::OpenUnread => {
                    view.window.present();
                    view.article_title.set_title("Unread");
                    *view.scope.borrow_mut() = OwnedScope::Unread;
                    view.manage.set_visible(false);
                    view.worker.send(Command::LoadArticles(OwnedScope::Unread));
                    view.outer.set_show_content(true);
                    view.inner.set_show_content(false);
                }
                IntegrationAction::Refresh => {
                    view.window.present();
                    if view.refresh_all.is_sensitive() {
                        view.refresh_all.set_sensitive(false);
                        view.network.send(NetworkCommand::RefreshAll);
                        view.toast.add_toast(adw::Toast::new("Refreshing feeds…"));
                    }
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

fn apply_event(view: &View, event: Event) {
    match event {
        Event::Navigation {
            feeds,
            folders,
            unread_by_feed,
            stats,
        } => {
            if let Some(integration) = &view.integration {
                integration.notify_unread_changed();
            }
            *view.feeds.borrow_mut() = feeds.clone();
            *view.folders.borrow_mut() = folders.clone();
            while let Some(row) = view.feed_list.row_at_index(0) {
                view.feed_list.remove(&row);
            }
            if let Some(row) = view.sidebar_list.row_at_index(0)
                && let Some(label) = row
                    .child()
                    .and_then(|c| c.downcast::<gtk::Box>().ok())
                    .and_then(|b| b.last_child())
                    .and_then(|c| c.downcast::<gtk::Label>().ok())
            {
                label.set_text(&stats.unread.to_string());
            }
            if let Some(row) = view.sidebar_list.row_at_index(2)
                && let Some(label) = row
                    .child()
                    .and_then(|c| c.downcast::<gtk::Box>().ok())
                    .and_then(|b| b.last_child())
                    .and_then(|c| c.downcast::<gtk::Label>().ok())
            {
                label.set_text(&stats.starred.to_string());
            }
            for folder in folders {
                let row = navigation_row(
                    "folder-symbolic",
                    &folder.name,
                    &format!("folder:{}", folder.id),
                    None,
                );
                install_folder_drop(&row, folder.id, view.worker.clone(), view.toast.clone());
                view.feed_list.append(&row);
            }
            let favicon_candidates = feeds
                .iter()
                .filter_map(|feed| {
                    let site_url = feed_icon_discovery_page(feed)?;
                    let mut requested = view.favicon_discovery_requested.borrow_mut();
                    requested
                        .insert(feed.stable_id.clone())
                        .then(|| (feed.stable_id.clone(), site_url))
                })
                .collect::<Vec<_>>();
            if !favicon_candidates.is_empty() {
                view.network
                    .send(NetworkCommand::DiscoverFavicons(favicon_candidates));
            }
            let unread_by_feed = unread_by_feed.into_iter().collect::<HashMap<_, _>>();
            for feed in feeds {
                let unread = unread_by_feed.get(&feed.stable_id).copied().unwrap_or(0);
                let row = feed_navigation_row(view, &feed, unread);
                install_feed_drag(&row, &feed.stable_id);
                view.feed_list.append(&row);
            }
        }
        Event::Articles {
            scope,
            items,
            next,
            append,
        } => {
            if !append
                && matches!(scope, OwnedScope::Unread)
                && matches!(*view.scope.borrow(), OwnedScope::Unread)
                && view.unread_snapshot_dirty.get()
            {
                return;
            }
            *view.scope.borrow_mut() = scope;
            request_article_image_enrichment(view, &items);
            populate_articles(view, items, next, append);
        }
        Event::SearchResults { query, items } => {
            view.article_title.set_title(&format!("Search: {query}"));
            request_article_image_enrichment(view, &items);
            populate_articles(view, items, None, false);
        }
        Event::Article(article) => show_article(view, *article),
        Event::ReadChanged { id, read } => {
            apply_article_state(view, &id, Some(read), None);
            if read && matches!(*view.scope.borrow(), OwnedScope::Unread) {
                view.unread_snapshot_dirty.set(true);
            }
            view.worker.send(Command::LoadNavigation);
        }
        Event::StarredChanged { id, starred } => {
            apply_article_state(view, &id, None, Some(starred));
            view.worker.send(Command::LoadNavigation);
        }
        Event::FeedRemoved => {
            view.toast.add_toast(adw::Toast::new("Feed unsubscribed"));
            *view.scope.borrow_mut() = OwnedScope::Unread;
            if let Some(row) = view.sidebar_list.row_at_index(0) {
                view.sidebar_list.select_row(Some(&row));
            }
            view.worker.send(Command::LoadNavigation);
            view.worker.send(Command::LoadArticles(OwnedScope::Unread));
        }
        Event::MutationComplete => {
            view.worker.send(Command::LoadNavigation);
            view.worker
                .send(Command::LoadArticles(view.scope.borrow().clone()));
        }
        Event::Notice(message) => {
            view.toast.add_toast(adw::Toast::new(&message));
            view.worker.send(Command::LoadNavigation);
            view.worker
                .send(Command::LoadArticles(view.scope.borrow().clone()));
        }
        Event::Error(error) => view.toast.add_toast(adw::Toast::new(&error)),
    }
}

fn apply_article_state(view: &View, stable_id: &str, read: Option<bool>, starred: Option<bool>) {
    if let Some(article) = view.open_article.borrow_mut().as_mut()
        && article.stable_id == stable_id
    {
        if let Some(read) = read {
            article.is_read = read;
        }
        if let Some(starred) = starred {
            article.is_starred = starred;
            view.reader_star.set_active(starred);
            view.reader_star.set_icon_name(if starred {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            });
        }
    }

    let Some(index) = view
        .article_items
        .borrow()
        .iter()
        .position(|item| item.stable_id == stable_id)
    else {
        return;
    };
    {
        let mut items = view.article_items.borrow_mut();
        let item = &mut items[index];
        if let Some(read) = read {
            item.is_unread = !read;
        }
        if let Some(starred) = starred {
            item.is_starred = starred;
        }
    }
    if let Some(state) = view.article_row_states.borrow().get(stable_id) {
        if let Some(read) = read {
            state.unread.set_visible(!read);
            state.is_unread.set(!read);
            if read {
                state.container.add_css_class("article-row-read");
                state.title.remove_css_class("article-title-unread");
                state.title.add_css_class("article-title");
            } else {
                state.container.remove_css_class("article-row-read");
                state.title.remove_css_class("article-title");
                state.title.add_css_class("article-title-unread");
            }
        }
        if let Some(starred) = starred {
            state.starred.set_visible(starred);
            state.is_starred.set(starred);
        }
    }
}

fn request_article_image_enrichment(view: &View, items: &[ArticleListItem]) {
    let mut requested = view.image_enrichment_requested.borrow_mut();
    let candidates = items
        .iter()
        .filter_map(|item| {
            if item.thumbnail_url.is_some() || !requested.insert(item.stable_id.clone()) {
                return None;
            }
            Some((item.stable_id.clone(), item.article_url.clone()?))
        })
        .take(24)
        .collect::<Vec<_>>();
    if !candidates.is_empty() {
        view.network
            .send(NetworkCommand::DiscoverArticleImages(candidates));
    }
}

fn populate_articles(
    view: &View,
    items: Vec<ArticleListItem>,
    next: Option<PageCursor>,
    append: bool,
) {
    if append {
        if view
            .article_ids
            .borrow()
            .last()
            .is_some_and(String::is_empty)
        {
            view.article_ids.borrow_mut().pop();
            if let Some(row) = view
                .article_list
                .row_at_index(view.article_ids.borrow().len() as i32)
            {
                view.article_list.remove(&row);
            }
        }
    } else {
        view.article_list.unselect_all();
        view.image_targets.borrow_mut().clear();
        while let Some(child) = view.article_list.first_child() {
            view.article_list.remove(&child);
        }
        view.article_ids.borrow_mut().clear();
        view.article_items.borrow_mut().clear();
        view.article_row_states.borrow_mut().clear();
    }
    view.article_items
        .borrow_mut()
        .extend(items.iter().cloned());
    for item in items {
        view.article_ids.borrow_mut().push(item.stable_id.clone());
        view.article_list.append(&article_row(view, &item));
    }
    *view.next_cursor.borrow_mut() = next;
    if view.next_cursor.borrow().is_some() {
        view.article_ids.borrow_mut().push(String::new());
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(
            &gtk::Label::builder()
                .label("Load more articles")
                .xalign(0.5)
                .css_classes(["load-more"])
                .build(),
        ));
        view.article_list.append(&row);
    }
    let article_count = view
        .article_ids
        .borrow()
        .iter()
        .filter(|id| !id.is_empty())
        .count();
    if article_count == 0 {
        view.empty.set_visible(true);
        view.article_scroller.set_visible(false);
    } else {
        view.empty.set_visible(false);
        view.article_scroller.set_visible(true);
    }
}

fn article_row(view: &View, item: &ArticleListItem) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    outer.add_css_class("article-row");
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 5);
    box_.set_hexpand(true);
    let meta = gtk::Label::builder()
        .label(format!(
            "{}  ·  {}",
            item.feed_name,
            article_time(item.published_at.or(item.updated_at), item.sort_timestamp)
        ))
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["article-meta"])
        .build();
    let title = gtk::Label::builder()
        .label(&item.title)
        .xalign(0.0)
        .wrap(true)
        .lines(2)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes([if item.is_unread {
            "article-title-unread"
        } else {
            "article-title"
        }])
        .build();
    box_.append(&meta);
    box_.append(&title);
    if let Some(summary) = item
        .summary
        .as_deref()
        .map(strip_html)
        .filter(|s| !s.is_empty())
    {
        box_.append(
            &gtk::Label::builder()
                .label(summary)
                .xalign(0.0)
                .wrap(true)
                .lines(2)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["article-summary"])
                .build(),
        );
    }
    let state = gtk::Box::new(gtk::Orientation::Vertical, 8);
    state.set_valign(gtk::Align::Center);
    state.set_width_request(16);
    let unread = gtk::Image::builder()
        .icon_name("media-record-symbolic")
        .tooltip_text("Unread")
        .visible(item.is_unread)
        .css_classes(["unread-dot"])
        .build();
    let starred = gtk::Image::builder()
        .icon_name("starred-symbolic")
        .tooltip_text("Starred")
        .visible(item.is_starred)
        .css_classes(["star-indicator"])
        .build();
    state.append(&unread);
    state.append(&starred);
    let is_unread = Rc::new(Cell::new(item.is_unread));
    let is_starred = Rc::new(Cell::new(item.is_starred));
    outer.append(&state);
    outer.append(&box_);
    let thumbnail = if item.thumbnail_url.is_some() || item.article_url.is_some() {
        let overlay = gtk::Overlay::new();
        overlay.set_size_request(96, 72);
        let picture = gtk::Picture::builder()
            .width_request(96)
            .height_request(72)
            .hexpand(false)
            .vexpand(false)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .content_fit(gtk::ContentFit::Cover)
            .can_shrink(true)
            // GtkPicture renders a missing-image glyph before a paintable is
            // available. Keep it transparent until decoding succeeds so an
            // absent or broken publisher image does not look like an app error.
            .opacity(0.0)
            .css_classes(["article-thumbnail", "image-placeholder"])
            .build();
        overlay.add_overlay(&picture);
        if let Some(url) = item.thumbnail_url.as_deref() {
            load_article_thumbnail(view, &picture, url);
        }
        outer.append(&overlay);
        Some(picture)
    } else {
        None
    };
    view.article_row_states.borrow_mut().insert(
        item.stable_id.clone(),
        ArticleRowState {
            container: outer.clone(),
            title: title.clone(),
            unread,
            starred,
            is_unread: Rc::clone(&is_unread),
            is_starred: Rc::clone(&is_starred),
            thumbnail,
        },
    );
    row.set_child(Some(&outer));
    install_article_context_menu(
        &row,
        item.stable_id.clone(),
        is_unread,
        is_starred,
        view.worker.clone(),
    );
    row
}

fn load_article_thumbnail(view: &View, picture: &gtk::Picture, url: &str) {
    let request = ImageRequest {
        url: url.to_owned(),
        width: 96,
        height: 72,
        fit: Fit::Cover,
    };
    view.image_targets
        .borrow_mut()
        .entry(request.clone())
        .or_default()
        .push(picture.clone());
    view.images.load(request);
}

fn show_article(view: &View, article: FullArticle) {
    let is_different_article = view
        .open_article
        .borrow()
        .as_ref()
        .is_none_or(|open| open.stable_id != article.stable_id);
    // Every article begins in the immediate RSS-native reader. Pages is an
    // explicit per-article choice, never a sticky default.
    view.scroll_mode.set_active(true);
    view.reader_title.set_text(&article.title);
    view.reader_title
        .set_cursor_from_name(article.url.as_ref().map(|_| "pointer"));
    let byline = article
        .author
        .as_deref()
        .map(|a| format!(" · {a}"))
        .unwrap_or_default();
    view.reader_meta.set_text(&format!(
        "{}{} · {}",
        article.feed_name,
        byline,
        article_time(
            article.published_at.or(article.updated_at),
            article.inserted_at
        )
    ));
    let source = article
        .content
        .as_deref()
        .or(article.summary.as_deref())
        .unwrap_or("");
    while let Some(child) = view.reader_content.first_child() {
        view.reader_content.remove(&child);
    }
    view.reader_image_targets.borrow_mut().clear();
    if let Some(url) = article.image_url.as_deref() {
        let hero = reader_picture(view, url, 1200, 640, 320, "reader-hero");
        view.reader_content.append(&hero);
    }
    match feedlizard_reader::parse_feed_html(source, article.url.as_deref()) {
        Ok(document) if !document.blocks.is_empty() => {
            for block in &document.blocks {
                render_reader_block(view, &view.reader_content, block.clone());
            }
            let mut page_blocks = Vec::with_capacity(document.blocks.len() + 1);
            page_blocks.push(Block::Heading {
                level: 1,
                text: article.title.clone(),
            });
            page_blocks.extend(document.blocks.clone());
            render_pages(
                view,
                &Document {
                    blocks: page_blocks,
                },
                article.url.as_deref(),
            );
        }
        _ => {
            view.reader_content.append(&reader_label(
                "This feed did not include readable article content. Open the original to read it.",
                "reader-body",
            ));
            render_pages(
                view,
                &Document { blocks: Vec::new() },
                article.url.as_deref(),
            );
        }
    }
    *view.open_article.borrow_mut() = Some(article.clone());
    if let Some(mut extracted) = view
        .extracted_articles
        .borrow()
        .get(&article.stable_id)
        .cloned()
    {
        extracted.blocks.insert(
            0,
            Block::Heading {
                level: 1,
                text: article.title.clone(),
            },
        );
        render_pages(view, &extracted, article.url.as_deref());
    } else if view.pages_mode.is_active() {
        request_full_article(view);
    }
    view.reader_star.set_active(article.is_starred);
    view.reader_star.set_icon_name(if article.is_starred {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    view.open_original.set_sensitive(article.url.is_some());
    if is_different_article {
        let adjustment = view.reader_scroller.vadjustment();
        adjustment.set_value(adjustment.lower());
        glib::idle_add_local_once(move || adjustment.set_value(adjustment.lower()));
    }
}

fn request_full_article(view: &View) {
    let Some(article) = view.open_article.borrow().clone() else {
        return;
    };
    let Some(url) = article.url else { return };
    if view
        .extracted_articles
        .borrow()
        .contains_key(&article.stable_id)
        || !view
            .article_extraction_requested
            .borrow_mut()
            .insert(article.stable_id.clone())
    {
        return;
    }
    view.network.send(NetworkCommand::ExtractArticle {
        article_id: article.stable_id,
        url,
    });
}

fn open_article_original(view: &View) {
    let url = view
        .open_article
        .borrow()
        .as_ref()
        .and_then(|article| article.url.clone());
    if let Some(url) = url {
        open_external_url(&view.toast, &url);
    }
}

fn open_external_url(toast: &adw::ToastOverlay, url: &str) {
    if let Err(error) = gio::AppInfo::launch_default_for_uri(url, None::<&gio::AppLaunchContext>) {
        toast.add_toast(adw::Toast::new(&format!("Could not open link: {error}")));
    }
}

fn render_pages(view: &View, document: &Document, article_url: Option<&str>) {
    while let Some(child) = view.pages_deck.first_child() {
        view.pages_deck.remove(&child);
    }
    // The Pages stack is hidden while Scroll is selected, so it may not have
    // an allocation yet. The containing mode stack is always allocated.
    let available_width = (view.reader_mode_stack.width() - 132).max(240);
    let available_height = (view.reader_mode_stack.height() - 220).max(240) as u32;
    let measure_widget = gtk::Label::new(None);
    let pages = feedlizard_reader::paginate(document, available_height, 16, 240, |text, style| {
        let layout = measure_widget.create_pango_layout(Some(text));
        layout.set_width(available_width * gtk::pango::SCALE);
        layout.set_wrap(gtk::pango::WrapMode::WordChar);
        let description = gtk::pango::FontDescription::from_string(match style {
            PageStyle::Heading(level) if level <= 2 => "Sans Bold 22",
            PageStyle::Heading(_) => "Sans Bold 18",
            PageStyle::Code => "Monospace 14",
            _ => {
                let size = (17.0 * view.reader_text_size.get() / 100.0).round() as u32;
                return {
                    let mut description = gtk::pango::FontDescription::new();
                    description.set_family("Sans");
                    description.set_size(size as i32 * gtk::pango::SCALE);
                    layout.set_font_description(Some(&description));
                    layout.pixel_size().1.max(1) as u32
                };
            }
        });
        layout.set_font_description(Some(&description));
        layout.pixel_size().1.max(1) as u32
    });
    for (index, page) in pages.iter().enumerate() {
        view.pages_deck.add_named(
            &page_widget(view, page, article_url),
            Some(&format!("page-{index}")),
        );
    }
    *view.pages.borrow_mut() = pages.clone();
    *view.page_article_url.borrow_mut() = article_url.map(str::to_owned);
    view.page_count.set(pages.len());
    view.book_view.set_sensitive(!pages.is_empty());
    show_page(view, 0);
}

fn page_widget(view: &View, page: &Page, article_url: Option<&str>) -> gtk::Widget {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_hexpand(true);
    content.set_halign(gtk::Align::Fill);
    content.add_css_class("page-paper");
    for chunk in &page.chunks {
        match chunk {
            PageChunk::Text { style, text } => {
                let class = match style {
                    PageStyle::Heading(level) if *level <= 2 => "reader-heading-large",
                    PageStyle::Heading(_) => "reader-heading",
                    PageStyle::Quote => "reader-quote",
                    PageStyle::Code => "reader-code",
                    _ => "reader-body",
                };
                let label = reader_label(text, class);
                if matches!(style, PageStyle::Heading(1))
                    && let Some(url) = article_url
                {
                    install_label_link(&label, url, &view.toast);
                }
                content.append(&label);
            }
            PageChunk::Link { text, url } => {
                let link = gtk::LinkButton::with_label(url, text);
                link.set_halign(gtk::Align::Start);
                link.set_hexpand(true);
                link.set_tooltip_text(Some(url));
                if let Some(label) = link
                    .child()
                    .and_then(|child| child.downcast::<gtk::Label>().ok())
                {
                    label.set_wrap(true);
                    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                    label.set_max_width_chars(72);
                    label.set_xalign(0.0);
                }
                content.append(&link);
            }
            PageChunk::Image { url, alt } => {
                let picture = reader_picture(view, url, 960, 600, 240, "page-image");
                picture.set_tooltip_text(Some(alt));
                content.append(&picture);
            }
        }
    }
    content.upcast()
}

fn show_page(view: &View, requested: usize) {
    let count = view.page_count.get();
    if count == 0 {
        view.pages_indicator.set_text("No pages");
        view.pages_previous.set_sensitive(false);
        view.pages_next.set_sensitive(false);
        return;
    }
    let index = requested.min(count - 1);
    view.page_index.set(index);
    view.pages_deck
        .set_visible_child_name(&format!("page-{index}"));
    view.pages_indicator
        .set_text(&format!("Page {} of {count}", index + 1));
    view.pages_previous.set_sensitive(index > 0);
    view.pages_next.set_sensitive(index + 1 < count);
}

struct BookSession {
    window: adw::ApplicationWindow,
    carousel_stack: gtk::Stack,
    wide: gtk::Stack,
    narrow: gtk::Stack,
    progress: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    current_page: Cell<usize>,
    page_count: usize,
}

fn enter_book_view(view: &Rc<View>) {
    if let Some(session) = view.book_session.borrow().as_ref() {
        session.window.present();
        return;
    }
    let pages = view.pages.borrow().clone();
    if pages.is_empty() {
        return;
    }
    let application = view.window.application().expect("application");
    let window = adw::ApplicationWindow::builder()
        .application(&application)
        .title("FeedLizard Book View")
        .default_width(1440)
        .default_height(900)
        .build();
    window.add_css_class("book-window");

    let wide = book_page_stack();
    let narrow = book_page_stack();
    for (spread_index, spread) in pages.chunks(2).enumerate() {
        let shell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        shell.add_css_class("book-spread");
        shell.set_halign(gtk::Align::Center);
        shell.set_valign(gtk::Align::Center);
        for (side, page) in spread.iter().enumerate() {
            let paper = page_widget(view, page, view.page_article_url.borrow().as_deref());
            paper.add_css_class("book-page-paper");
            paper.add_css_class(if side == 0 {
                "book-page-left"
            } else {
                "book-page-right"
            });
            shell.append(&paper);
        }
        if spread.len() == 1 {
            shell.add_css_class("book-spread-single");
        }
        wide.add_named(&shell, Some(&format!("spread-{spread_index}")));
    }
    for (page_index, page) in pages.iter().enumerate() {
        let shell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        shell.add_css_class("book-spread");
        shell.add_css_class("book-spread-single");
        shell.set_halign(gtk::Align::Center);
        shell.set_valign(gtk::Align::Center);
        let paper = page_widget(view, page, view.page_article_url.borrow().as_deref());
        paper.add_css_class("book-page-paper");
        shell.append(&paper);
        narrow.add_named(&shell, Some(&format!("single-{page_index}")));
    }

    let carousel_stack = gtk::Stack::new();
    carousel_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    carousel_stack.add_named(&wide, Some("wide"));
    carousel_stack.add_named(&narrow, Some("narrow"));
    carousel_stack.set_visible_child_name("wide");

    let exit = gtk::Button::builder()
        .icon_name("view-restore-symbolic")
        .tooltip_text("Exit Book View (Esc)")
        .build();
    let title = gtk::Label::builder()
        .label(
            view.open_article
                .borrow()
                .as_ref()
                .map(|article| article.title.as_str())
                .unwrap_or("Pages"),
        )
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .hexpand(true)
        .xalign(0.0)
        .css_classes(["book-title"])
        .build();
    let progress = gtk::Label::builder().css_classes(["book-progress"]).build();
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    controls.add_css_class("book-controls");
    controls.append(&exit);
    controls.append(&title);
    controls.append(&progress);
    let controls_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::Crossfade)
        .transition_duration(180)
        .reveal_child(true)
        .child(&controls)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Start)
        .build();

    let previous = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Previous page (Left Arrow)")
        .css_classes(["circular", "book-page-arrow"])
        .build();
    previous.update_property(&[gtk::accessible::Property::Label("Previous page")]);
    let next = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text("Next page (Right Arrow)")
        .css_classes(["circular", "book-page-arrow"])
        .build();
    next.update_property(&[gtk::accessible::Property::Label("Next page")]);
    let bottom_navigation = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    bottom_navigation.add_css_class("book-bottom-navigation");
    bottom_navigation.set_halign(gtk::Align::Center);
    bottom_navigation.set_valign(gtk::Align::End);
    bottom_navigation.append(&previous);
    bottom_navigation.append(&next);

    let overlay = gtk::Overlay::new();
    overlay.add_css_class("book-environment");
    overlay.set_child(Some(&carousel_stack));
    overlay.add_overlay(&controls_revealer);
    overlay.add_overlay(&bottom_navigation);
    window.set_content(Some(&overlay));
    if let Ok(condition) = adw::BreakpointCondition::parse("max-width: 1050px") {
        let breakpoint = adw::Breakpoint::new(condition);
        breakpoint.add_setter(
            &carousel_stack,
            "visible-child-name",
            Some(&"narrow".to_value()),
        );
        window.add_breakpoint(breakpoint);
    }

    let session = Rc::new(BookSession {
        window: window.clone(),
        carousel_stack,
        wide,
        narrow,
        progress,
        previous,
        next,
        current_page: Cell::new(view.page_index.get().min(pages.len() - 1)),
        page_count: pages.len(),
    });
    book_set_page(&session, session.current_page.get(), false);
    connect_book_view(view, &session, &overlay, &controls_revealer, &exit);
    *view.book_session.borrow_mut() = Some(Rc::clone(&session));
    window.present();
    window.fullscreen();
}

fn book_page_stack() -> gtk::Stack {
    let stack = gtk::Stack::new();
    stack.set_transition_type(if animations_enabled() {
        gtk::StackTransitionType::SlideLeftRight
    } else {
        gtk::StackTransitionType::None
    });
    stack.set_transition_duration(220);
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack
}

fn connect_book_view(
    view: &Rc<View>,
    session: &Rc<BookSession>,
    overlay: &gtk::Overlay,
    controls: &gtk::Revealer,
    exit: &gtk::Button,
) {
    let weak_session = Rc::downgrade(session);
    session.previous.connect_clicked(move |_| {
        if let Some(session) = weak_session.upgrade() {
            book_navigate(&session, -1);
        }
    });
    let weak_session = Rc::downgrade(session);
    session.next.connect_clicked(move |_| {
        if let Some(session) = weak_session.upgrade() {
            book_navigate(&session, 1);
        }
    });

    let window = session.window.clone();
    exit.connect_clicked(move |_| window.close());
    let keys = gtk::EventControllerKey::new();
    let weak_session = Rc::downgrade(session);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(session) = weak_session.upgrade() else {
            return glib::Propagation::Proceed;
        };
        match key {
            gtk::gdk::Key::Escape | gtk::gdk::Key::F11 => {
                session.window.close();
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Right | gtk::gdk::Key::Page_Down => {
                book_navigate(&session, 1);
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Left | gtk::gdk::Key::Page_Up => {
                book_navigate(&session, -1);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    session.window.add_controller(keys);

    let click = gtk::GestureClick::new();
    let weak_session = Rc::downgrade(session);
    click.connect_released(move |gesture, _, x, _| {
        let Some(session) = weak_session.upgrade() else {
            return;
        };
        let width = gesture.widget().map(|widget| widget.width()).unwrap_or(1) as f64;
        if x < width * 0.10 {
            book_navigate(&session, -1);
        } else if x > width * 0.90 {
            book_navigate(&session, 1);
        }
    });
    overlay.add_controller(click);

    // Carousel dragging can lose the gesture to selectable page content. This
    // capture-phase fallback makes a deliberate horizontal page drag reliable
    // while leaving short drags available for text selection.
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    drag.connect_drag_update(|gesture, offset_x, offset_y| {
        if offset_x.abs() >= 16.0 && offset_x.abs() > offset_y.abs() * 1.25 {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    let weak_session = Rc::downgrade(session);
    drag.connect_drag_end(move |_, offset_x, offset_y| {
        let Some(session) = weak_session.upgrade() else {
            return;
        };
        if offset_x.abs() >= 72.0 && offset_x.abs() > offset_y.abs() * 1.25 {
            book_navigate(&session, if offset_x < 0.0 { 1 } else { -1 });
        }
    });
    overlay.add_controller(drag);

    let generation = Rc::new(Cell::new(0_u64));
    let motion = gtk::EventControllerMotion::new();
    let revealer = controls.clone();
    let generation_for_motion = Rc::clone(&generation);
    motion.connect_motion(move |_, _, _| {
        if !revealer.reveals_child() {
            revealer.set_reveal_child(true);
            schedule_book_controls_hide(&revealer, Rc::clone(&generation_for_motion));
        }
    });
    overlay.add_controller(motion);
    schedule_book_controls_hide(controls, generation);

    let weak_view = Rc::downgrade(view);
    let weak_session = Rc::downgrade(session);
    session.window.connect_close_request(move |_| {
        if let (Some(view), Some(session)) = (weak_view.upgrade(), weak_session.upgrade()) {
            show_page(&view, session.current_page.get());
            view.book_session.borrow_mut().take();
        }
        glib::Propagation::Proceed
    });
}

fn book_navigate(session: &BookSession, direction: i32) {
    let wide = session.carousel_stack.visible_child_name().as_deref() == Some("wide");
    let target = book_navigation_target(
        session.current_page.get(),
        direction,
        wide,
        session.page_count,
    );
    book_set_page(session, target, animations_enabled());
}

fn book_navigation_target(current: usize, direction: i32, wide: bool, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let step = if wide { 2 } else { 1 };
    (current as i64 + i64::from(direction) * step as i64).clamp(0, count.saturating_sub(1) as i64)
        as usize
}

fn book_set_page(session: &BookSession, requested: usize, animate: bool) {
    let page = requested.min(session.page_count.saturating_sub(1));
    session.current_page.set(page);
    let transition = if animate {
        gtk::StackTransitionType::SlideLeftRight
    } else {
        gtk::StackTransitionType::None
    };
    session.wide.set_transition_type(transition);
    session.narrow.set_transition_type(transition);
    session
        .wide
        .set_visible_child_name(&format!("spread-{}", page / 2));
    session
        .narrow
        .set_visible_child_name(&format!("single-{page}"));
    book_update_position(session, page);
}

fn book_update_position(session: &BookSession, page: usize) {
    let page = page.min(session.page_count.saturating_sub(1));
    session.current_page.set(page);
    let wide = session.carousel_stack.visible_child_name().as_deref() == Some("wide");
    session
        .progress
        .set_text(&book_progress_label(page, session.page_count, wide));
    session.previous.set_sensitive(page > 0);
    let step = if wide { 2 } else { 1 };
    session.next.set_sensitive(page + step < session.page_count);
}

fn book_progress_label(page: usize, count: usize, wide: bool) -> String {
    let page = page.min(count.saturating_sub(1));
    if wide && page + 1 < count {
        format!("Pages {}–{} of {}", page + 1, page + 2, count)
    } else {
        format!("Page {} of {}", page + 1, count)
    }
}

fn animations_enabled() -> bool {
    gtk::Settings::default()
        .map(|settings| settings.is_gtk_enable_animations())
        .unwrap_or(true)
}

fn schedule_book_controls_hide(revealer: &gtk::Revealer, generation: Rc<Cell<u64>>) {
    let marker = generation.get().wrapping_add(1);
    generation.set(marker);
    let revealer = revealer.clone();
    glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
        if generation.get() == marker && animations_enabled() {
            revealer.set_reveal_child(false);
        }
    });
}

fn render_reader_block(view: &View, container: &gtk::Box, block: Block) {
    match block {
        Block::Heading { level, text } => container.append(&reader_label(
            &text,
            if level <= 2 {
                "reader-heading-large"
            } else {
                "reader-heading"
            },
        )),
        Block::Paragraph { text, links } => {
            container.append(&reader_label(&text, "reader-body"));
            if !links.is_empty() {
                let link_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
                link_box.add_css_class("reader-links");
                for link in links.into_iter().take(8) {
                    let button = gtk::LinkButton::with_label(&link.url, &link.text);
                    button.set_halign(gtk::Align::Start);
                    button.set_tooltip_text(Some(&link.url));
                    link_box.append(&button);
                }
                container.append(&link_box);
            }
        }
        Block::Quote(text) => container.append(&reader_label(&text, "reader-quote")),
        Block::Code(text) => container.append(&reader_label(&text, "reader-code")),
        Block::ListItem(text) => {
            container.append(&reader_label(&format!("•  {text}"), "reader-body"))
        }
        Block::Image { url, alt } => {
            let picture = reader_picture(view, &url, 1200, 800, 360, "reader-inline-image");
            picture.set_tooltip_text(Some(&alt));
            container.append(&picture);
        }
    }
}

fn reader_picture(
    view: &View,
    url: &str,
    width: u32,
    height: u32,
    display_height: i32,
    class: &str,
) -> gtk::Picture {
    let picture = gtk::Picture::builder()
        .height_request(display_height)
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .css_classes([class, "image-placeholder"])
        .build();
    let request = ImageRequest {
        url: url.to_owned(),
        width,
        height,
        fit: Fit::Contain,
    };
    view.reader_image_targets
        .borrow_mut()
        .entry(request.clone())
        .or_default()
        .push(picture.clone());
    view.images.load(request);
    picture
}

fn reader_label(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .hexpand(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(88)
        .selectable(true)
        .xalign(0.0)
        .yalign(0.0)
        .css_classes([class])
        .build()
}

fn install_label_link(label: &gtk::Label, url: &str, toast: &adw::ToastOverlay) {
    label.set_cursor_from_name(Some("pointer"));
    label.add_css_class("reader-link");
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    let url = url.to_owned();
    let toast = toast.clone();
    click.connect_released(move |_, _, _, _| open_external_url(&toast, &url));
    label.add_controller(click);
}

fn install_smooth_wheel_scroll(scroller: &gtk::ScrolledWindow) {
    scroller.set_kinetic_scrolling(true);
    let controller = gtk::EventControllerScroll::new(
        gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE,
    );
    let adjustment = scroller.vadjustment();
    let target_value = Rc::new(Cell::new(adjustment.value()));
    let active_animation = Rc::new(RefCell::new(None::<adw::TimedAnimation>));
    let widget = scroller.clone();
    controller.connect_scroll(move |_, _, dy| {
        let lower = adjustment.lower();
        let upper = (adjustment.upper() - adjustment.page_size()).max(lower);
        let target = (target_value.get() + dy * 112.0).clamp(lower, upper);
        target_value.set(target);
        if !gtk::Settings::default().is_some_and(|settings| settings.is_gtk_enable_animations()) {
            adjustment.set_value(target);
            return glib::Propagation::Stop;
        }
        if let Some(animation) = active_animation.borrow_mut().take() {
            animation.pause();
        }
        let start = adjustment.value();
        let animated_adjustment = adjustment.clone();
        let animation = adw::TimedAnimation::new(
            &widget,
            start,
            target,
            180,
            adw::CallbackAnimationTarget::new(move |value| animated_adjustment.set_value(value)),
        );
        animation.set_easing(adw::Easing::EaseOutCubic);
        animation.play();
        *active_animation.borrow_mut() = Some(animation);
        glib::Propagation::Stop
    });
    scroller.add_controller(controller);
}

fn navigation_row(icon: &str, title: &str, tag: &str, count: Option<i64>) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_tooltip_text(Some(tag));
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    box_.add_css_class("navigation-row");
    box_.append(&gtk::Image::from_icon_name(icon));
    box_.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    let badge = gtk::Label::builder()
        .label(count.unwrap_or_default().to_string())
        .css_classes(["count-badge"])
        .visible(count.is_some())
        .build();
    box_.append(&badge);
    row.set_child(Some(&box_));
    row
}

fn feed_navigation_row(view: &View, feed: &FeedRecord, unread: i64) -> gtk::ListBoxRow {
    let row = navigation_row(
        "application-rss+xml-symbolic",
        &feed.display_name,
        &format!("feed:{}", feed.stable_id),
        Some(unread),
    );
    install_feed_context_menu(view, &row, feed);
    let Some(icon_url) = feed_icon_url(feed) else {
        return row;
    };
    let Some(content) = row
        .child()
        .and_then(|child| child.downcast::<gtk::Box>().ok())
    else {
        return row;
    };
    if let Some(generic) = content.first_child() {
        content.remove(&generic);
    }
    let overlay = gtk::Overlay::new();
    overlay.set_size_request(22, 22);
    overlay.set_child(Some(&gtk::Image::from_icon_name(
        "application-rss+xml-symbolic",
    )));
    let picture = gtk::Picture::builder()
        .width_request(22)
        .height_request(22)
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .opacity(0.0)
        .css_classes(["feed-icon"])
        .build();
    overlay.add_overlay(&picture);
    content.prepend(&overlay);
    let request = ImageRequest {
        url: icon_url,
        width: 44,
        height: 44,
        fit: Fit::Cover,
    };
    if let Some(texture) = view.icon_textures.borrow().get(&request) {
        picture.set_paintable(Some(texture));
        picture.set_opacity(1.0);
        picture.remove_css_class("image-placeholder");
    } else {
        view.icon_targets
            .borrow_mut()
            .entry(request.clone())
            .or_default()
            .push(picture);
        view.icons.load(request);
    }
    row
}

fn install_feed_context_menu(view: &View, row: &gtk::ListBoxRow, feed: &FeedRecord) {
    let click = gtk::GestureClick::new();
    click.set_button(3);
    let row_for_popover = row.clone();
    let window = view.window.clone();
    let worker = view.worker.clone();
    let feed_id = feed.stable_id.clone();
    let feed_name = feed.display_name.clone();
    click.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let unsubscribe = gtk::Button::builder()
            .label("Unsubscribe…")
            .css_classes(["flat", "destructive-action"])
            .build();
        let popover = gtk::Popover::builder().has_arrow(true).child(&unsubscribe).build();
        popover.set_parent(&row_for_popover);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let popover_to_close = popover.clone();
        let window = window.clone();
        let worker = worker.clone();
        let feed_id = feed_id.clone();
        let feed_name = feed_name.clone();
        unsubscribe.connect_clicked(move |_| {
            popover_to_close.popdown();
            let dialog = adw::AlertDialog::builder()
                .heading(format!("Unsubscribe from {feed_name}?"))
                .body("The feed and its locally stored articles will be removed. This cannot be undone.")
                .build();
            dialog.add_responses(&[("cancel", "Cancel"), ("unsubscribe", "Unsubscribe")]);
            dialog.set_response_appearance(
                "unsubscribe",
                adw::ResponseAppearance::Destructive,
            );
            let worker = worker.clone();
            let feed_id = feed_id.clone();
            dialog.connect_response(Some("unsubscribe"), move |_, _| {
                worker.send(Command::RemoveFeed(feed_id.clone()));
            });
            dialog.present(Some(&window));
        });
        popover.popup();
    });
    row.add_controller(click);
}

fn feed_icon_url(feed: &FeedRecord) -> Option<String> {
    feed.favicon_url
        .clone()
        .or_else(|| feed.feed_image_url.clone())
        .or_else(|| site_favicon_url(feed.site_url.as_deref()))
}

fn site_favicon_url(site_url: Option<&str>) -> Option<String> {
    let mut site = url::Url::parse(site_url?).ok()?;
    if !matches!(site.scheme(), "http" | "https") {
        return None;
    }
    site.set_path("/favicon.ico");
    site.set_query(None);
    site.set_fragment(None);
    Some(site.to_string())
}

fn feed_icon_discovery_page(feed: &FeedRecord) -> Option<String> {
    if let Some(site_url) = &feed.site_url {
        return Some(site_url.clone());
    }
    let mut url = url::Url::parse(&feed.normalized_url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

fn install_feed_drag(row: &gtk::ListBoxRow, feed_id: &str) {
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    let feed_id = feed_id.to_owned();
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&feed_id.to_value()))
    });
    row.add_controller(source);
}

fn install_article_context_menu(
    row: &gtk::ListBoxRow,
    article_id: String,
    is_unread: Rc<Cell<bool>>,
    is_starred: Rc<Cell<bool>>,
    worker: Worker,
) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let menu_parent = row.clone();
    gesture.connect_pressed(move |_, _, x, y| {
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
        menu.set_margin_top(6);
        menu.set_margin_bottom(6);
        menu.set_margin_start(6);
        menu.set_margin_end(6);
        let star = gtk::Button::builder()
            .label(if is_starred.get() {
                "Remove Star"
            } else {
                "Star"
            })
            .halign(gtk::Align::Fill)
            .build();
        star.add_css_class("flat");
        let read = gtk::Button::builder()
            .label(if is_unread.get() {
                "Mark as Read"
            } else {
                "Mark as Unread"
            })
            .halign(gtk::Align::Fill)
            .build();
        read.add_css_class("flat");
        menu.append(&star);
        menu.append(&read);
        let popover = gtk::Popover::builder().autohide(true).child(&menu).build();
        popover.set_parent(&menu_parent);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let id = article_id.clone();
        let worker_for_star = worker.clone();
        let starred = Rc::clone(&is_starred);
        let popup = popover.downgrade();
        star.connect_clicked(move |_| {
            worker_for_star.send(Command::SetStarred {
                id: id.clone(),
                starred: !starred.get(),
            });
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
            }
        });
        let id = article_id.clone();
        let worker_for_read = worker.clone();
        let unread = Rc::clone(&is_unread);
        let popup = popover.downgrade();
        read.connect_clicked(move |_| {
            worker_for_read.send(Command::SetRead {
                id: id.clone(),
                read: unread.get(),
            });
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
            }
        });
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    });
    row.add_controller(gesture);
}

fn install_starred_context_menu(view: &Rc<View>, row: &gtk::ListBoxRow) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let weak_view = Rc::downgrade(view);
    let menu_parent = row.clone();
    gesture.connect_pressed(move |_, _, x, y| {
        let Some(view) = weak_view.upgrade() else { return };
        let button = gtk::Button::builder()
            .label("Unstar All…")
            .halign(gtk::Align::Fill)
            .css_classes(["flat"])
            .build();
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
        menu.set_margin_top(6);
        menu.set_margin_bottom(6);
        menu.set_margin_start(6);
        menu.set_margin_end(6);
        menu.append(&button);
        let popover = gtk::Popover::builder().autohide(true).child(&menu).build();
        popover.set_parent(&menu_parent);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let popup = popover.downgrade();
        let weak_view = Rc::downgrade(&view);
        button.connect_clicked(move |_| {
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
            }
            let Some(view) = weak_view.upgrade() else { return };
            let dialog = adw::AlertDialog::builder()
                .heading("Unstar every article?")
                .body("This removes every star from your library. Articles and read status are not changed.")
                .build();
            dialog.add_responses(&[("cancel", "Cancel"), ("unstar", "Unstar All")]);
            dialog.set_response_appearance("unstar", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let worker = view.worker.clone();
            dialog.connect_response(Some("unstar"), move |_, _| {
                worker.send(Command::UnstarAll);
            });
            dialog.present(Some(&view.window));
        });
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    });
    row.add_controller(gesture);
}

fn install_folder_drop(
    row: &gtk::ListBoxRow,
    folder_id: i64,
    worker: Worker,
    toast: adw::ToastOverlay,
) {
    let target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let highlight = row.clone();
    target.connect_enter(move |_, _, _| {
        highlight.add_css_class("drop-target-active");
        gtk::gdk::DragAction::MOVE
    });
    let highlight = row.clone();
    target.connect_leave(move |_| highlight.remove_css_class("drop-target-active"));
    let highlight = row.clone();
    target.connect_drop(move |_, value, _, _| {
        highlight.remove_css_class("drop-target-active");
        let Ok(feed_id) = value.get::<String>() else {
            return false;
        };
        if !feed_id.starts_with("feed:v1:") {
            return false;
        }
        worker.send(Command::MoveFeed {
            id: feed_id,
            folder_id: Some(folder_id),
        });
        toast.add_toast(adw::Toast::new("Feed moved to folder"));
        true
    });
    row.add_controller(target);
}

fn scope_title(scope: &OwnedScope) -> &str {
    match scope {
        OwnedScope::Library => "Library",
        OwnedScope::Unread => "Unread",
        OwnedScope::Starred => "Starred",
        OwnedScope::Feed(_) => "Feed",
        OwnedScope::Folder(_) => "Folder",
    }
}
fn article_time(published_at: Option<i64>, inserted_at: i64) -> String {
    let timestamp = published_at.unwrap_or(inserted_at);
    let Some(time) = chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|value| value.with_timezone(&chrono::Local))
    else {
        return "Unknown time".into();
    };
    let formatted = format_article_datetime(time, chrono::Local::now());
    if published_at.is_some() {
        formatted
    } else {
        format!("Added {formatted}")
    }
}

fn format_article_datetime<Tz>(time: chrono::DateTime<Tz>, now: chrono::DateTime<Tz>) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let date = if time.date_naive() == now.date_naive() {
        "Today".to_owned()
    } else if now.date_naive().pred_opt() == Some(time.date_naive()) {
        "Yesterday".to_owned()
    } else if time.date_naive().year() == now.date_naive().year() {
        time.format("%b %-d").to_string()
    } else {
        time.format("%b %-d, %Y").to_string()
    };
    let hour = match time.hour() % 12 {
        0 => 12,
        hour => hour,
    };
    let period = if time.hour() < 12 { "AM" } else { "PM" };
    format!("{date} · {hour}:{:02} {period}", time.minute())
}
fn strip_html(input: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for ch in input.chars() {
        match ch {
            '<' => tag = true,
            '>' => {
                tag = false;
                out.push(' ');
            }
            _ if !tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
pub(crate) fn database_path() -> PathBuf {
    std::env::var_os("FEEDLIZARD_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
                .unwrap_or_else(|| PathBuf::from("."));
            base.join("feedlizard/library.sqlite3")
        })
}

fn image_cache_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("feedlizard/images")
}

fn settings_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("feedlizard/reader-text-size")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Appearance {
    System,
    Light,
    Dark,
}

impl Appearance {
    fn index(self) -> u32 {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }

    fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Light,
            2 => Self::Dark,
            _ => Self::System,
        }
    }
}

fn appearance_path() -> PathBuf {
    settings_path().with_file_name("appearance")
}

fn load_appearance() -> Appearance {
    match std::fs::read_to_string(appearance_path())
        .as_deref()
        .map(str::trim)
    {
        Ok("light") => Appearance::Light,
        Ok("dark") => Appearance::Dark,
        _ => Appearance::System,
    }
}

fn save_appearance(appearance: Appearance) {
    let path = appearance_path();
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_ok()
    {
        let value = match appearance {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        };
        let _ = std::fs::write(path, format!("{value}\n"));
    }
}

fn apply_appearance(appearance: Appearance) {
    adw::StyleManager::default().set_color_scheme(match appearance {
        Appearance::System => adw::ColorScheme::Default,
        Appearance::Light => adw::ColorScheme::ForceLight,
        Appearance::Dark => adw::ColorScheme::ForceDark,
    });
}

fn load_reader_text_size() -> f64 {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .map(|value| value.clamp(80.0, 160.0))
        .unwrap_or(100.0)
}

fn save_reader_text_size(value: f64) {
    let path = settings_path();
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_ok()
    {
        let _ = std::fs::write(path, format!("{value:.0}\n"));
    }
}

fn install_reader_text_size(percent: f64) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(
        ".reader-body {{ font-size: {:.3}rem; }} .reader-title {{ font-size: {:.3}rem; }}",
        1.12 * percent / 100.0,
        2.0 * percent.sqrt() / 10.0
    ));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
}

fn window_dimension(name: &str, default: i32) -> i32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .map(|value: i32| value.clamp(360, 3840))
        .unwrap_or(default)
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(r#"
      .navigation-sidebar { padding: 8px; background: alpha(@view_fg_color, .025); }
      .navigation-sidebar row { border-radius: 9px; margin: 2px 0; }
      .navigation-sidebar row.drop-target-active { background: alpha(@accent_color, .20); outline: 2px solid alpha(@accent_color, .65); outline-offset: -2px; }
      .navigation-row { padding: 9px 10px; }
      .feed-icon { border-radius: 5px; }
      .section-heading { font-size: .72rem; font-weight: 700; letter-spacing: .08em; opacity: .55; padding: 20px 10px 6px; }
      .count-badge { border-radius: 999px; padding: 1px 8px; background: alpha(@accent_color, .15); color: @accent_color; font-weight: 600; }
      .article-list { background: @view_bg_color; }
      .article-list row { border-bottom: 1px solid alpha(@view_fg_color, .08); }
      .article-row { padding: 14px 18px; transition: opacity 140ms ease; }
      .article-row-read { opacity: .62; }
      .article-meta, .reader-meta { color: alpha(@view_fg_color, .62); font-size: .88rem; }
      .article-title { font-size: 1.04rem; } .article-title-unread { font-size: 1.04rem; font-weight: 700; }
      .unread-dot { color: @accent_color; -gtk-icon-size: 9px; }
      .star-indicator { color: #e5a50a; -gtk-icon-size: 15px; }
      .article-summary { color: alpha(@view_fg_color, .70); }
      .load-more { padding: 16px; color: @accent_color; font-weight: 600; }
      .article-thumbnail { border-radius: 8px; min-width: 96px; min-height: 72px; }
      .image-placeholder { background: alpha(@view_fg_color, .07); }
      .reader-page { padding: 52px 72px; }
      .reader-title { font-size: 2rem; font-weight: 750; line-height: 1.12; }
      .reader-title:hover, .reader-link:hover { color: @accent_color; }
      .reader-content { margin-top: 18px; }
      .reader-hero, .reader-inline-image, .page-image { border-radius: 10px; }
      .reader-body { font-size: 1.12rem; line-height: 1.55; }
      .reader-heading-large { font-size: 1.5rem; font-weight: 700; margin-top: 18px; }
      .reader-heading { font-size: 1.25rem; font-weight: 700; margin-top: 14px; }
      .reader-quote { font-size: 1.08rem; font-style: italic; color: alpha(@view_fg_color, .78); border-left: 3px solid @accent_color; padding-left: 18px; }
      .reader-code { font-family: monospace; background: alpha(@view_fg_color, .06); border-radius: 8px; padding: 14px; }
      .reader-links { margin-top: -8px; }
      .pages-shell { background: alpha(@view_fg_color, .035); }
      .page-paper { margin: 18px 24px; padding: 38px 42px; border-radius: 5px; background: @view_bg_color; box-shadow: 0 3px 16px alpha(black, .14); }
      .pages-footer { padding: 10px 18px; border-top: 1px solid alpha(@view_fg_color, .08); }
      .book-window, .book-environment { background: @window_bg_color; }
      .book-controls { margin: 18px; padding: 8px 10px; border-radius: 12px; background: alpha(@window_bg_color, .94); box-shadow: 0 4px 22px alpha(black, .22); }
      .book-bottom-navigation { margin: 24px; padding: 7px 12px; border-radius: 999px; background: alpha(@window_bg_color, .92); box-shadow: 0 4px 20px alpha(black, .20); }
      .book-page-arrow { min-width: 44px; min-height: 44px; }
      .book-title { font-weight: 650; }
      .book-progress { color: alpha(@window_fg_color, .68); }
      .book-spread { min-height: 690px; padding: 72px 38px 38px; }
      .book-spread .page-paper { margin: 0; min-width: 420px; min-height: 590px; padding: 54px 58px; border-radius: 3px; }
      .book-spread:not(.book-spread-single) .book-page-left { border-radius: 5px 1px 1px 5px; box-shadow: -8px 10px 30px alpha(black, .18), inset -12px 0 20px alpha(black, .045); }
      .book-spread:not(.book-spread-single) .book-page-right { border-radius: 1px 5px 5px 1px; box-shadow: 8px 10px 30px alpha(black, .18), inset 12px 0 20px alpha(black, .035); }
      .book-spread:not(.book-spread-single) .book-page-left { border-right: 1px solid alpha(@view_fg_color, .06); }
      .book-spread:not(.book-spread-single) .book-page-right { border-left: 1px solid alpha(black, .08); }
      .book-spread-single .page-paper { min-width: 460px; border-radius: 5px; box-shadow: 0 12px 36px alpha(black, .20); }
    "#);
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

#[cfg(test)]
mod tests {
    use super::{
        book_navigation_target, book_progress_label, format_article_datetime,
        format_display_version, format_snapshot_datetime, site_favicon_url,
    };
    use chrono::{FixedOffset, TimeZone};

    #[test]
    fn snapshot_time_uses_the_presented_timezone() {
        let eastern = FixedOffset::west_opt(4 * 60 * 60).unwrap();
        let time = eastern.timestamp_opt(1_777_000_000, 0).unwrap();
        assert_eq!(format_snapshot_datetime(time), "April 23, 2026 at 11:06 PM");
    }

    #[test]
    fn article_time_includes_local_date_context_and_clock_time() {
        let eastern = FixedOffset::west_opt(4 * 60 * 60).unwrap();
        let now = eastern.with_ymd_and_hms(2026, 8, 30, 15, 0, 0).unwrap();
        let today = eastern.with_ymd_and_hms(2026, 8, 30, 9, 30, 0).unwrap();
        let yesterday = eastern.with_ymd_and_hms(2026, 8, 29, 21, 0, 0).unwrap();
        let older = eastern.with_ymd_and_hms(2026, 8, 20, 16, 12, 0).unwrap();
        assert_eq!(format_article_datetime(today, now), "Today · 9:30 AM");
        assert_eq!(
            format_article_datetime(yesterday, now),
            "Yesterday · 9:00 PM"
        );
        assert_eq!(format_article_datetime(older, now), "Aug 20 · 4:12 PM");
    }

    #[test]
    fn derives_private_favicon_fallback_from_site_origin() {
        assert_eq!(
            site_favicon_url(Some("https://example.com/news?view=all#latest")),
            Some("https://example.com/favicon.ico".into())
        );
        assert_eq!(site_favicon_url(Some("file:///tmp/site")), None);
        assert_eq!(site_favicon_url(None), None);
    }

    #[test]
    fn book_navigation_respects_spreads_and_boundaries() {
        assert_eq!(book_navigation_target(0, -1, true, 5), 0);
        assert_eq!(book_navigation_target(0, 1, true, 5), 2);
        assert_eq!(book_navigation_target(2, 1, true, 5), 4);
        assert_eq!(book_navigation_target(4, 1, true, 5), 4);
        assert_eq!(book_navigation_target(2, -1, true, 5), 0);
        assert_eq!(book_navigation_target(2, 1, false, 5), 3);
        assert_eq!(book_navigation_target(0, 1, false, 0), 0);
    }

    #[test]
    fn book_progress_describes_spreads_and_single_pages() {
        assert_eq!(book_progress_label(0, 5, true), "Pages 1–2 of 5");
        assert_eq!(book_progress_label(4, 5, true), "Page 5 of 5");
        assert_eq!(book_progress_label(2, 5, false), "Page 3 of 5");
    }

    #[test]
    fn formats_prerelease_versions_for_about_without_changing_stable_versions() {
        assert_eq!(format_display_version("0.9.0-beta.12"), "0.9.0 Beta 12");
        assert_eq!(format_display_version("1.0.0-rc.1"), "1.0.0 RC 1");
        assert_eq!(format_display_version("1.0.0"), "1.0.0");
    }
}
