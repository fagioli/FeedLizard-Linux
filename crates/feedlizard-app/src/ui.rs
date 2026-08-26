use crate::{
    image_worker::{Event as ImageEvent, ImageWorker},
    network_worker::{Command as NetworkCommand, Event as NetworkEvent, NetworkWorker},
    nostr_worker::{Command as NostrCommand, Event as NostrEvent, NostrWorker, SnapshotSummary},
    omarchy,
    worker::{Command, Event, OwnedScope, Worker},
};
use adw::prelude::*;
use feedlizard_image::{Fit, Request as ImageRequest};
use feedlizard_integration::{
    IntegrationAction, IntegrationHandle, start_service as start_integration_service,
};
use feedlizard_reader::{Block, Document, Page, PageChunk, PageStyle};
use feedlizard_storage::{ArticleListItem, FeedRecord, FolderRecord, FullArticle, PageCursor};
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
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
    article_list: gtk::ListBox,
    article_title: adw::WindowTitle,
    empty: adw::StatusPage,
    article_scroller: gtk::ScrolledWindow,
    reader_title: gtk::Label,
    reader_meta: gtk::Label,
    reader_content: gtk::Box,
    pages_deck: gtk::Stack,
    pages_indicator: gtk::Label,
    pages_previous: gtk::Button,
    pages_next: gtk::Button,
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
    integration: Option<IntegrationHandle>,
    scope: RefCell<OwnedScope>,
    article_ids: RefCell<Vec<String>>,
    open_article: RefCell<Option<FullArticle>>,
    feeds: RefCell<Vec<FeedRecord>>,
    folders: RefCell<Vec<FolderRecord>>,
    image_targets: RefCell<HashMap<String, Vec<gtk::Picture>>>,
    page_count: Cell<usize>,
    page_index: Cell<usize>,
    reader_text_size: Cell<f64>,
    next_cursor: RefCell<Option<PageCursor>>,
}

pub fn build_window(application: &adw::Application) {
    if let Some(window) = application.active_window() {
        window.present();
        return;
    }
    install_css();
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
    let (images, image_events) = ImageWorker::start(image_cache_path());
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
        integration,
    ));
    connect_view(&view);
    poll_events(&view, events);
    poll_network_events(&view, network_events);
    poll_nostr_events(&view, nostr_events);
    poll_image_events(&view, image_events);
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
            (tag == "scope:unread").then_some(0),
        ));
    }
    sidebar_list.append(&separator_row("Feeds"));

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
        .child(&sidebar_list)
        .build();
    sidebar_toolbar.set_content(Some(&sidebar_scroll));

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
        .xalign(0.0)
        .css_classes(["reader-title"])
        .build();
    let reader_meta = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["reader-meta"])
        .build();
    let reader_content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    reader_content.add_css_class("reader-content");
    let reader_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
    reader_box.add_css_class("reader-page");
    reader_box.append(&reader_title);
    reader_box.append(&reader_meta);
    reader_box.append(&reader_content);
    let reader_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&reader_box)
        .build();
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
    reader_header.pack_end(&open_original);
    reader_header.pack_end(&reader_star);
    let reader = adw::ToolbarView::new();
    reader.add_top_bar(&reader_header);
    reader.set_content(Some(&reader_mode_stack));

    let inner = adw::NavigationSplitView::new();
    inner.set_min_sidebar_width(330.0);
    inner.set_max_sidebar_width(540.0);
    inner.set_sidebar_width_fraction(0.40);
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
        article_list,
        article_title,
        empty,
        article_scroller,
        reader_title,
        reader_meta,
        reader_content,
        pages_deck,
        pages_indicator,
        pages_previous,
        pages_next,
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
        integration,
        scope: RefCell::new(OwnedScope::Unread),
        article_ids: RefCell::new(Vec::new()),
        open_article: RefCell::new(None),
        feeds: RefCell::new(Vec::new()),
        folders: RefCell::new(Vec::new()),
        image_targets: RefCell::new(HashMap::new()),
        page_count: Cell::new(0),
        page_index: Cell::new(0),
        reader_text_size: Cell::new(reader_text_size),
        next_cursor: RefCell::new(None),
    }
}

fn connect_view(view: &Rc<View>) {
    let weak = Rc::downgrade(view);
    view.sidebar_list.connect_row_activated(move |_, row| {
        let Some(view) = weak.upgrade() else { return };
        let Some(tag) = row.tooltip_text() else {
            return;
        };
        if tag == "settings" {
            show_settings(&view);
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
        *view.scope.borrow_mut() = scope.clone();
        view.manage
            .set_visible(matches!(scope, OwnedScope::Feed(_) | OwnedScope::Folder(_)));
        view.worker.send(Command::LoadArticles(scope));
        view.outer.set_show_content(true);
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
        let Some(view) = weak.upgrade() else { return };
        if let Some(url) = view
            .open_article
            .borrow()
            .as_ref()
            .and_then(|article| article.url.as_deref())
            && let Err(error) =
                gio::AppInfo::launch_default_for_uri(url, None::<&gio::AppLaunchContext>)
        {
            view.toast
                .add_toast(adw::Toast::new(&format!("Could not open article: {error}")));
        }
    });
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
    Back,
}

fn perform_window_action(view: &View, action: WindowAction) {
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
    let add = gtk::Button::builder()
        .label("Add Feed")
        .css_classes(["suggested-action"])
        .build();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
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
            .title("Omarchy Integration")
            .subtitle("Add FeedLizard to your Omarchy bar for unread counts and quick access")
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

fn show_omarchy_integration(view: &Rc<View>) {
    let command = omarchy::install_command();
    let body = if command.is_some() {
        "Omarchy will show its normal security warning and ask you to approve the plugin. FeedLizard never writes to your Omarchy configuration directly."
    } else {
        "The official plugin source is prepared, but its public repository has not been published yet. FeedLizard will use Omarchy’s normal confirmed installation workflow when it becomes available."
    };
    let dialog = adw::AlertDialog::builder()
        .heading("Omarchy Integration")
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
    glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
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
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
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
                NetworkEvent::Error(error) => view.toast.add_toast(adw::Toast::new(&error)),
            }
        }
        glib::ControlFlow::Continue
    });
}

fn poll_nostr_events(view: &Rc<View>, events: Receiver<NostrEvent>) {
    let view = Rc::clone(view);
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
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
    glib::timeout_add_local(std::time::Duration::from_millis(32), move || {
        if !view.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        while let Ok(event) = events.try_recv() {
            match event {
                ImageEvent::Loaded { url, image } => {
                    let bytes = glib::Bytes::from_owned(image.rgba);
                    let texture = gtk::gdk::MemoryTexture::new(
                        image.width as i32,
                        image.height as i32,
                        gtk::gdk::MemoryFormat::R8g8b8a8,
                        &bytes,
                        (image.width * 4) as usize,
                    );
                    if let Some(targets) = view.image_targets.borrow_mut().remove(&url) {
                        for picture in targets {
                            picture.set_paintable(Some(&texture));
                            picture.remove_css_class("image-placeholder");
                        }
                    }
                }
                ImageEvent::Failed { url } => {
                    view.image_targets.borrow_mut().remove(&url);
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

fn poll_integration_actions(view: &Rc<View>, actions: Receiver<IntegrationAction>) {
    let view = Rc::clone(view);
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
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
            stats,
        } => {
            if let Some(integration) = &view.integration {
                integration.notify_unread_changed();
            }
            *view.feeds.borrow_mut() = feeds.clone();
            *view.folders.borrow_mut() = folders.clone();
            while let Some(row) = view.sidebar_list.row_at_index(5) {
                view.sidebar_list.remove(&row);
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
            for folder in folders {
                view.sidebar_list.append(&navigation_row(
                    "folder-symbolic",
                    &folder.name,
                    &format!("folder:{}", folder.id),
                    None,
                ));
            }
            for feed in feeds {
                view.sidebar_list.append(&navigation_row(
                    "application-rss+xml-symbolic",
                    &feed.display_name,
                    &format!("feed:{}", feed.stable_id),
                    None,
                ));
            }
        }
        Event::Articles {
            scope,
            items,
            next,
            append,
        } => {
            *view.scope.borrow_mut() = scope;
            populate_articles(view, items, next, append);
        }
        Event::SearchResults { query, items } => {
            view.article_title.set_title(&format!("Search: {query}"));
            populate_articles(view, items, None, false);
        }
        Event::Article(article) => show_article(view, *article),
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
    }
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
            relative_time(item.published_at)
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
    if item.is_unread {
        state.append(
            &gtk::Image::builder()
                .icon_name("media-record-symbolic")
                .tooltip_text("Unread")
                .css_classes(["unread-dot"])
                .build(),
        );
    }
    if item.is_starred {
        state.append(
            &gtk::Image::builder()
                .icon_name("starred-symbolic")
                .tooltip_text("Starred")
                .css_classes(["star-indicator"])
                .build(),
        );
    }
    if state.first_child().is_some() {
        outer.append(&state);
    }
    outer.append(&box_);
    if let Some(url) = item.thumbnail_url.as_deref() {
        let picture = gtk::Picture::builder()
            .width_request(96)
            .height_request(72)
            .content_fit(gtk::ContentFit::Cover)
            .can_shrink(true)
            .css_classes(["article-thumbnail", "image-placeholder"])
            .build();
        view.image_targets
            .borrow_mut()
            .entry(url.to_owned())
            .or_default()
            .push(picture.clone());
        view.images.load(ImageRequest {
            url: url.to_owned(),
            width: 192,
            height: 144,
            fit: Fit::Cover,
        });
        outer.append(&picture);
    }
    row.set_child(Some(&outer));
    row
}

fn show_article(view: &View, article: FullArticle) {
    view.reader_title.set_text(&article.title);
    let byline = article
        .author
        .as_deref()
        .map(|a| format!(" · {a}"))
        .unwrap_or_default();
    view.reader_meta.set_text(&format!(
        "{}{} · {}",
        article.feed_name,
        byline,
        relative_time(article.published_at)
    ));
    let source = article
        .content
        .as_deref()
        .or(article.summary.as_deref())
        .unwrap_or("");
    while let Some(child) = view.reader_content.first_child() {
        view.reader_content.remove(&child);
    }
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
            );
        }
        _ => {
            view.reader_content.append(&reader_label(
                "This feed did not include readable article content. Open the original to read it.",
                "reader-body",
            ));
            render_pages(view, &Document { blocks: Vec::new() });
        }
    }
    view.reader_star.set_active(article.is_starred);
    view.reader_star.set_icon_name(if article.is_starred {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    view.open_original.set_sensitive(article.url.is_some());
    *view.open_article.borrow_mut() = Some(article);
}

fn render_pages(view: &View, document: &Document) {
    while let Some(child) = view.pages_deck.first_child() {
        view.pages_deck.remove(&child);
    }
    let available_width = (view.pages_deck.width() - 144).max(360);
    let available_height = (view.pages_deck.height() - 100).max(480) as u32;
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
        view.pages_deck
            .add_named(&page_widget(view, page), Some(&format!("page-{index}")));
    }
    view.page_count.set(pages.len());
    show_page(view, 0);
}

fn page_widget(view: &View, page: &Page) -> gtk::Widget {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
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
                content.append(&reader_label(text, class));
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
                let link_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                link_box.add_css_class("reader-links");
                for link in links.into_iter().take(8) {
                    let button = gtk::LinkButton::with_label(&link.url, &link.text);
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
    view.image_targets
        .borrow_mut()
        .entry(url.to_owned())
        .or_default()
        .push(picture.clone());
    view.images.load(ImageRequest {
        url: url.to_owned(),
        width,
        height,
        fit: Fit::Contain,
    });
    picture
}

fn reader_label(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .yalign(0.0)
        .css_classes([class])
        .build()
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

fn separator_row(title: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_child(Some(
        &gtk::Label::builder()
            .label(title.to_uppercase())
            .xalign(0.0)
            .css_classes(["section-heading"])
            .build(),
    ));
    row
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
fn relative_time(timestamp: Option<i64>) -> String {
    timestamp
        .map(|t| {
            let days = (unix_now() - t).max(0) / 86_400;
            match days {
                0 => "Today".into(),
                1 => "Yesterday".into(),
                n if n < 7 => format!("{n} days ago"),
                _ => format!("{days}d"),
            }
        })
        .unwrap_or_else(|| "Unknown time".into())
}
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
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
fn database_path() -> PathBuf {
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
      .navigation-row { padding: 9px 10px; }
      .section-heading { font-size: .72rem; font-weight: 700; letter-spacing: .08em; opacity: .55; padding: 20px 10px 6px; }
      .count-badge { border-radius: 999px; padding: 1px 8px; background: alpha(@accent_color, .15); color: @accent_color; font-weight: 600; }
      .article-list { background: @view_bg_color; }
      .article-list row { border-bottom: 1px solid alpha(@view_fg_color, .08); }
      .article-row { padding: 14px 18px; }
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
      .reader-content { margin-top: 18px; }
      .reader-hero, .reader-inline-image, .page-image { border-radius: 10px; }
      .reader-body { font-size: 1.12rem; line-height: 1.55; }
      .reader-heading-large { font-size: 1.5rem; font-weight: 700; margin-top: 18px; }
      .reader-heading { font-size: 1.25rem; font-weight: 700; margin-top: 14px; }
      .reader-quote { font-size: 1.08rem; font-style: italic; color: alpha(@view_fg_color, .78); border-left: 3px solid @accent_color; padding-left: 18px; }
      .reader-code { font-family: monospace; background: alpha(@view_fg_color, .06); border-radius: 8px; padding: 14px; }
      .reader-links { margin-top: -8px; }
      .pages-shell { background: alpha(@view_fg_color, .035); }
      .page-paper { margin: 28px 48px; padding: 52px 64px; border-radius: 5px; background: @view_bg_color; box-shadow: 0 3px 16px alpha(black, .14); }
      .pages-footer { padding: 10px 18px; border-top: 1px solid alpha(@view_fg_color, .08); }
    "#);
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

#[cfg(test)]
mod tests {
    use super::format_snapshot_datetime;
    use chrono::{FixedOffset, TimeZone};

    #[test]
    fn snapshot_time_uses_the_presented_timezone() {
        let eastern = FixedOffset::west_opt(4 * 60 * 60).unwrap();
        let time = eastern.timestamp_opt(1_777_000_000, 0).unwrap();
        assert_eq!(format_snapshot_datetime(time), "April 23, 2026 at 11:06 PM");
    }
}
