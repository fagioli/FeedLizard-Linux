use crate::{
    network_worker::{Command as NetworkCommand, Event as NetworkEvent, NetworkWorker},
    ui,
    worker::{Command, Event, OwnedScope, Worker},
};
use adw::prelude::*;
use feedlizard_reader::Block;
use feedlizard_storage::{ArticleListItem, FullArticle};
use gtk::{gdk, gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc::Receiver,
};

const PURPLE: &str = "#9b7cff";
const MAX_SIGNAL_ROWS: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeState {
    Activity,
    Flatline,
    Scanning,
    Interrupted,
}

struct View {
    window: adw::ApplicationWindow,
    article_list: gtk::ListBox,
    content_stack: gtk::Stack,
    list_scroller: gtk::ScrolledWindow,
    empty_state: gtk::Box,
    preview: gtk::Box,
    preview_title: gtk::Label,
    preview_meta: gtk::Label,
    preview_body: gtk::Label,
    preview_original: gtk::LinkButton,
    unread_count: gtk::Label,
    feed_count: gtk::Label,
    feed_count_name: gtk::Label,
    sync_value: gtk::Label,
    sync_detail: gtk::Label,
    status: gtk::Label,
    help: gtk::Box,
    scope_state: Rc<Cell<ScopeState>>,
    scope_frame: Rc<Cell<u8>>,
    scope_generation: Rc<Cell<u64>>,
    scope_levels: Rc<RefCell<Vec<f64>>>,
    scope: gtk::DrawingArea,
    worker: Worker,
    network: NetworkWorker,
    articles: RefCell<Vec<ArticleListItem>>,
    open_article: RefCell<Option<FullArticle>>,
}

pub fn install_actions(application: &adw::Application) {
    application.add_action_entries([gio::ActionEntry::builder("quit")
        .activate(|app: &adw::Application, _, _| app.quit())
        .build()]);
    application.set_accels_for_action("app.quit", &["<primary>q"]);
}

pub fn build_window(application: &adw::Application) {
    if let Some(window) = application.active_window() {
        window.present();
        return;
    }
    install_css();
    let path = ui::database_path();
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        eprintln!("FeedLizard could not create its data directory: {error}");
        application.quit();
        return;
    }
    let (worker, events) = Worker::start(path.clone());
    let (network, network_events) = NetworkWorker::start(path);
    let view = Rc::new(build_view(application, worker, network));
    connect_view(&view);
    poll_storage(&view, events);
    poll_network(&view, network_events);
    let keepalive = view.clone();
    view.window.connect_destroy(move |_| {
        let _ = &keepalive;
    });
    view.worker.send(Command::LoadNavigation);
    view.worker.send(Command::LoadArticles(OwnedScope::Unread));
    view.window.present();
}

fn build_view(application: &adw::Application, worker: Worker, network: NetworkWorker) -> View {
    let scope_state = Rc::new(Cell::new(ScopeState::Flatline));
    let scope_frame = Rc::new(Cell::new(0));
    let scope_generation = Rc::new(Cell::new(0));
    let scope_levels = Rc::new(RefCell::new(vec![0.0; 18]));
    let scope = signal_scope(
        scope_state.clone(),
        scope_frame.clone(),
        scope_levels.clone(),
        116,
        58,
    );

    let brand = gtk::Label::builder()
        .label("▰ FEEDLIZARD")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["signals-brand"])
        .build();
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.add_css_class("signals-header");
    header.append(&brand);
    header.append(&scope);

    let unread_metric = metric("0", "UNREAD");
    let feed_metric = metric("0", "FEEDS");
    let unread_count = metric_value(&unread_metric);
    let feed_count = metric_value(&feed_metric);
    let feed_count_name = metric_name(&feed_metric);
    let sync_metric = status_metric("IDLE", "LOCAL DATA");
    let sync_value = metric_value(&sync_metric);
    let sync_detail = metric_name(&sync_metric);
    let metrics = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    metrics.add_css_class("signals-metrics");
    metrics.append(&unread_metric);
    metrics.append(&feed_metric);
    metrics.append(&sync_metric);

    let section = gtk::Label::builder()
        .label("LATEST UNREAD")
        .xalign(0.0)
        .css_classes(["signals-section"])
        .build();

    let article_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(false)
        .css_classes(["signals-list"])
        .build();
    let list_scroller = gtk::ScrolledWindow::builder()
        .child(&article_list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(330)
        .max_content_height(470)
        .vexpand(true)
        .build();

    let empty_scope = signal_scope(
        Rc::new(Cell::new(ScopeState::Flatline)),
        Rc::new(Cell::new(0)),
        Rc::new(RefCell::new(vec![0.0; 18])),
        180,
        76,
    );
    let empty_state = gtk::Box::new(gtk::Orientation::Vertical, 8);
    empty_state.set_valign(gtk::Align::Center);
    empty_state.set_vexpand(true);
    empty_state.add_css_class("signals-empty");
    empty_state.append(&empty_scope);
    empty_state.append(
        &gtk::Label::builder()
            .label("ALL CAUGHT UP")
            .css_classes(["signals-empty-title"])
            .build(),
    );
    empty_state.append(
        &gtk::Label::builder()
            .label("THE LIZARD SLEEPS")
            .css_classes(["signals-empty-copy"])
            .build(),
    );

    let preview_title = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["signals-preview-title"])
        .build();
    let preview_meta = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["signals-meta"])
        .build();
    let preview_body = gtk::Label::builder()
        .xalign(0.0)
        .yalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .lines(12)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .selectable(true)
        .css_classes(["signals-preview-body"])
        .build();
    let preview_original = gtk::LinkButton::builder()
        .label("OPEN ORIGINAL ↗")
        .halign(gtk::Align::Start)
        .css_classes(["signals-link"])
        .build();
    let preview_back = gtk::Button::builder()
        .label("← BACK TO ARTICLES")
        .halign(gtk::Align::Start)
        .css_classes(["signals-flat-button"])
        .build();
    let preview = gtk::Box::new(gtk::Orientation::Vertical, 14);
    preview.set_margin_top(18);
    preview.set_margin_bottom(20);
    preview.set_margin_start(20);
    preview.set_margin_end(20);
    preview.append(&preview_back);
    preview.append(&preview_title);
    preview.append(&preview_meta);
    preview.append(&preview_body);
    preview.append(&preview_original);

    let content_stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(100)
        .build();
    content_stack.add_named(&list_scroller, Some("signals"));
    content_stack.add_named(&empty_state, Some("empty"));
    content_stack.add_named(&preview, Some("preview"));
    content_stack.set_visible_child_name("empty");

    let status = gtk::Label::builder()
        .label("J/K NAV  //  ↵ PREVIEW  //  S STAR  //  O OPEN  //  ? HELP")
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["signals-status-copy"])
        .build();
    let open_full = gtk::Button::builder()
        .label("[ OPEN FEEDLIZARD ]")
        .css_classes(["signals-open-full"])
        .build();
    let status_bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    status_bar.add_css_class("signals-status");
    status_bar.append(&status);
    status_bar.append(&open_full);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&metrics);
    root.append(&section);
    root.append(&content_stack);
    root.append(&status_bar);

    let help = help_overlay();
    help.set_visible(false);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&root));
    overlay.add_overlay(&help);

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("FeedLizard")
        .default_width(620)
        .default_height(580)
        .content(&overlay)
        .build();
    window.set_size_request(430, 460);
    window.add_css_class("signals-window");

    let view = View {
        window,
        article_list,
        content_stack,
        list_scroller,
        empty_state,
        preview,
        preview_title,
        preview_meta,
        preview_body,
        preview_original,
        unread_count,
        feed_count,
        feed_count_name,
        sync_value,
        sync_detail,
        status,
        help,
        scope_state,
        scope_frame,
        scope_generation,
        scope_levels,
        scope,
        worker,
        network,
        articles: RefCell::new(Vec::new()),
        open_article: RefCell::new(None),
    };

    let content_stack = view.content_stack.clone();
    preview_back.connect_clicked(move |_| content_stack.set_visible_child_name("signals"));
    let window = view.window.clone();
    let launch_status = view.status.clone();
    open_full.connect_clicked(move |_| match launch_full_application() {
        Ok(()) => window.close(),
        Err(error) => launch_status.set_text(&format!("LAUNCH FAILED // {}", concise(&error, 48))),
    });
    view
}

fn connect_view(view: &Rc<View>) {
    view.article_list.connect_row_activated(glib::clone!(
        #[weak]
        view,
        move |_, row| preview_row(&view, row.index())
    ));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        view,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, modifiers| {
            if modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::j | gdk::Key::Down => select_relative(&view, 1),
                gdk::Key::k | gdk::Key::Up => select_relative(&view, -1),
                gdk::Key::Return => {
                    if view.content_stack.visible_child_name().as_deref() == Some("signals")
                        && let Some(row) = view.article_list.selected_row()
                    {
                        preview_row(&view, row.index());
                    }
                }
                gdk::Key::r => toggle_read(&view),
                gdk::Key::s => toggle_star(&view),
                gdk::Key::o => open_original(&view),
                gdk::Key::R => refresh(&view),
                gdk::Key::question => view.help.set_visible(!view.help.is_visible()),
                gdk::Key::Escape | gdk::Key::q => {
                    if view.help.is_visible() {
                        view.help.set_visible(false);
                    } else if view.content_stack.visible_child_name().as_deref() == Some("preview")
                    {
                        view.content_stack.set_visible_child_name("signals");
                    } else {
                        view.window.close();
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        }
    ));
    view.window.add_controller(keys);
}

fn apply_event(view: &Rc<View>, event: Event) {
    match event {
        Event::Navigation { stats, .. } => {
            view.unread_count.set_text(&stats.unread.to_string());
            view.feed_count.set_text(&stats.feeds.to_string());
            view.feed_count_name
                .set_text(if stats.feeds == 1 { "FEED" } else { "FEEDS" });
        }
        Event::Articles { items, append, .. } => {
            if !append {
                populate_articles(view, items);
                view.status
                    .set_text("READY  //  J/K NAV  //  ↵ PREVIEW  //  ? HELP");
            }
        }
        Event::Article(article) => show_preview(view, *article),
        Event::ReadChanged { id, read } => {
            if let Some(article) = view
                .articles
                .borrow_mut()
                .iter_mut()
                .find(|a| a.stable_id == id)
            {
                article.is_unread = !read;
            }
            refresh_article_row(view, &id);
            view.status.set_text(if read {
                "ARTICLE MARKED READ"
            } else {
                "ARTICLE MARKED UNREAD"
            });
            view.worker.send(Command::LoadNavigation);
        }
        Event::StarredChanged { id, starred } => {
            if let Some(article) = view
                .articles
                .borrow_mut()
                .iter_mut()
                .find(|a| a.stable_id == id)
            {
                article.is_starred = starred;
            }
            refresh_article_row(view, &id);
            view.status.set_text(if starred {
                "ARTICLE STARRED"
            } else {
                "ARTICLE UNSTARRED"
            });
        }
        Event::Notice(message) => view.status.set_text(&message.to_uppercase()),
        Event::Error(error) => {
            set_sync(view, "ERROR", "LOCAL DATA SAFE", ScopeState::Interrupted);
            view.status
                .set_text(&format!("ERROR // {}", concise(&error, 58)));
        }
        Event::MutationComplete | Event::FeedRemoved | Event::SearchResults { .. } => {}
    }
}

fn populate_articles(view: &Rc<View>, items: Vec<ArticleListItem>) {
    while let Some(row) = view.article_list.row_at_index(0) {
        view.article_list.remove(&row);
    }
    let items = items.into_iter().take(MAX_SIGNAL_ROWS).collect::<Vec<_>>();
    *view.articles.borrow_mut() = items.clone();
    if items.is_empty() {
        view.empty_state.set_visible(true);
        view.content_stack.set_visible_child_name("empty");
        *view.scope_levels.borrow_mut() = vec![0.0; 18];
        set_scope(view, ScopeState::Flatline);
        return;
    }
    *view.scope_levels.borrow_mut() = signal_levels(&items, unix_now(), 18);
    set_scope(view, ScopeState::Activity);
    for item in &items {
        view.article_list.append(&article_row(item));
    }
    view.list_scroller.set_visible(true);
    view.content_stack.set_visible_child_name("signals");
    if let Some(row) = view.article_list.row_at_index(0) {
        view.article_list.select_row(Some(&row));
    }
}

fn preview_row(view: &Rc<View>, index: i32) {
    let Some(article) = view.articles.borrow().get(index as usize).cloned() else {
        return;
    };
    view.worker
        .send(Command::OpenArticle(article.stable_id.clone()));
    if article.is_unread {
        view.worker.send(Command::SetRead {
            id: article.stable_id,
            read: true,
        });
    }
}

fn show_preview(view: &Rc<View>, article: FullArticle) {
    view.preview_title.set_text(&article.title);
    view.preview_meta.set_text(&format!(
        "{}  //  {}",
        article.feed_name.to_uppercase(),
        relative_time(
            article
                .published_at
                .or(article.updated_at)
                .unwrap_or(article.inserted_at)
        )
    ));
    let source = article
        .content
        .as_deref()
        .or(article.summary.as_deref())
        .unwrap_or("");
    let text = feedlizard_reader::parse_feed_html(source, article.url.as_deref())
        .ok()
        .map(|document| {
            document
                .blocks
                .into_iter()
                .filter_map(block_text)
                .take(14)
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            "No readable feed preview. Open FeedLizard or the original article to continue.".into()
        });
    view.preview_body.set_text(&text);
    view.preview_original
        .set_uri(article.url.as_deref().unwrap_or("about:blank"));
    view.preview_original.set_visible(article.url.is_some());
    *view.open_article.borrow_mut() = Some(article);
    view.preview.set_visible(true);
    view.content_stack.set_visible_child_name("preview");
    view.status
        .set_text("PREVIEW  //  ESC BACK  //  O OPEN ORIGINAL");
}

fn block_text(block: Block) -> Option<String> {
    match block {
        Block::Heading { text, .. }
        | Block::Quote(text)
        | Block::Code(text)
        | Block::ListItem(text)
        | Block::Paragraph { text, .. }
            if !text.is_empty() =>
        {
            Some(text)
        }
        _ => None,
    }
}

fn poll_storage(view: &Rc<View>, events: Receiver<Event>) {
    glib::timeout_add_local(
        std::time::Duration::from_millis(16),
        glib::clone!(
            #[weak]
            view,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                for event in events.try_iter().take(64) {
                    apply_event(&view, event);
                }
                glib::ControlFlow::Continue
            }
        ),
    );
}

fn poll_network(view: &Rc<View>, events: Receiver<NetworkEvent>) {
    glib::timeout_add_local(
        std::time::Duration::from_millis(50),
        glib::clone!(
            #[weak]
            view,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                for event in events.try_iter().take(32) {
                    match event {
                        NetworkEvent::RefreshComplete(summary) => {
                            let state = if summary.failed == 0 {
                                "SYNCED"
                            } else {
                                "DEGRADED"
                            };
                            let scope = if summary.failed == 0 {
                                ScopeState::Activity
                            } else {
                                ScopeState::Interrupted
                            };
                            let detail = if summary.failed == 0 {
                                "JUST NOW".to_owned()
                            } else {
                                format!("{} FAILED", summary.failed)
                            };
                            set_sync(&view, state, &detail, scope);
                            view.status.set_text(&format!(
                                "SYNC {}  //  {} COMPLETE  //  {} FAILED",
                                if summary.failed == 0 {
                                    "COMPLETE"
                                } else {
                                    "DEGRADED"
                                },
                                summary.completed,
                                summary.failed
                            ));
                            view.worker.send(Command::LoadNavigation);
                            view.worker.send(Command::LoadArticles(OwnedScope::Unread));
                        }
                        NetworkEvent::Error(error) => {
                            set_sync(&view, "ERROR", "FETCH FAILED", ScopeState::Interrupted);
                            view.status
                                .set_text(&format!("FETCH FAILED // {}", concise(&error, 58)));
                        }
                        _ => {}
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );
}

fn select_relative(view: &Rc<View>, delta: i32) {
    if view.content_stack.visible_child_name().as_deref() != Some("signals") {
        return;
    }
    let count = view.articles.borrow().len() as i32;
    if count == 0 {
        return;
    }
    let current = view
        .article_list
        .selected_row()
        .map_or(-1, |row| row.index());
    let next = (current + delta).clamp(0, count - 1);
    if let Some(row) = view.article_list.row_at_index(next) {
        view.article_list.select_row(Some(&row));
        row.grab_focus();
    }
}

fn selected(view: &View) -> Option<ArticleListItem> {
    let index = view.article_list.selected_row()?.index() as usize;
    view.articles.borrow().get(index).cloned()
}

fn toggle_read(view: &Rc<View>) {
    if let Some(article) = selected(view) {
        view.worker.send(Command::SetRead {
            id: article.stable_id,
            read: article.is_unread,
        });
    }
}

fn toggle_star(view: &Rc<View>) {
    if let Some(article) = selected(view) {
        view.worker.send(Command::SetStarred {
            id: article.stable_id,
            starred: !article.is_starred,
        });
    }
}

fn open_original(view: &Rc<View>) {
    let url = view
        .open_article
        .borrow()
        .as_ref()
        .and_then(|article| article.url.clone())
        .or_else(|| selected(view).and_then(|article| article.article_url));
    let Some(url) = url else { return };
    if gio::AppInfo::launch_default_for_uri(&url, None::<&gio::AppLaunchContext>).is_ok() {
        view.status.set_text("OPENED IN BROWSER");
    }
}

fn launch_full_application() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    std::process::Command::new(executable)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn refresh(view: &Rc<View>) {
    let detail = format!("{} FEEDS", view.feed_count.text());
    set_sync(view, "REFRESHING", &detail, ScopeState::Scanning);
    view.status.set_text("REFRESHING…");
    view.network.send(NetworkCommand::RefreshAll);
}

fn set_sync(view: &View, value: &str, detail: &str, state: ScopeState) {
    view.sync_value.set_text(value);
    view.sync_detail.set_text(detail);
    set_scope(view, state);
}

fn set_scope(view: &View, state: ScopeState) {
    let state = if state == ScopeState::Scanning && !animations_enabled() {
        ScopeState::Activity
    } else {
        state
    };
    let generation = view.scope_generation.get().wrapping_add(1);
    view.scope_generation.set(generation);
    view.scope_frame.set(0);
    view.scope_state.set(state);
    view.scope.queue_draw();

    if state == ScopeState::Scanning {
        for (frame, delay_ms) in [(1, 80), (2, 150), (3, 220), (4, 290), (5, 360), (6, 430)] {
            schedule_scope_frame(view, generation, frame, delay_ms);
        }
        schedule_scope_state(view, generation, ScopeState::Activity, 520);
    }
}

fn animations_enabled() -> bool {
    gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
}

fn schedule_scope_frame(view: &View, generation: u64, frame: u8, delay_ms: u64) {
    let area = view.scope.downgrade();
    let current_generation = view.scope_generation.clone();
    let animation_frame = view.scope_frame.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(delay_ms), move || {
        if current_generation.get() == generation {
            animation_frame.set(frame);
            if let Some(area) = area.upgrade() {
                area.queue_draw();
            }
        }
    });
}

fn schedule_scope_state(view: &View, generation: u64, state: ScopeState, delay_ms: u64) {
    let area = view.scope.downgrade();
    let current_generation = view.scope_generation.clone();
    let scope_state = view.scope_state.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(delay_ms), move || {
        if current_generation.get() == generation {
            scope_state.set(state);
            if let Some(area) = area.upgrade() {
                area.queue_draw();
            }
        }
    });
}

fn metric(value: &str, name: &str) -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 1);
    box_.set_hexpand(true);
    box_.add_css_class("signals-metric");
    box_.append(
        &gtk::Label::builder()
            .label(value)
            .xalign(0.0)
            .css_classes(["signals-metric-value"])
            .build(),
    );
    box_.append(
        &gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .css_classes(["signals-metric-name"])
            .build(),
    );
    box_
}

fn metric_value(metric: &gtk::Box) -> gtk::Label {
    metric
        .first_child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
        .expect("metric value label")
}

fn metric_name(metric: &gtk::Box) -> gtk::Label {
    metric
        .last_child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
        .expect("metric name label")
}

fn status_metric(value: &str, name: &str) -> gtk::Box {
    let box_ = metric(value, name);
    box_.add_css_class("signals-health");
    box_
}

fn article_row(item: &ArticleListItem) -> gtk::ListBoxRow {
    gtk::ListBoxRow::builder()
        .child(&article_row_content(item))
        .css_classes(["signals-row"])
        .build()
}

fn article_row_content(item: &ArticleListItem) -> gtk::Box {
    let marker = gtk::Label::builder()
        .label(if item.is_unread { "●" } else { "○" })
        .valign(gtk::Align::Start)
        .css_classes(if item.is_unread {
            vec!["signals-unread"]
        } else {
            vec!["signals-read-marker"]
        })
        .build();
    let title = gtk::Label::builder()
        .label(&item.title)
        .xalign(0.0)
        .wrap(true)
        .lines(2)
        .hexpand(true)
        .css_classes(if item.is_unread {
            vec!["signals-title"]
        } else {
            vec!["signals-title", "read"]
        })
        .build();
    let time = gtk::Label::builder()
        .label(relative_time(item.sort_timestamp))
        .xalign(1.0)
        .valign(gtk::Align::Start)
        .width_chars(4)
        .css_classes(["signals-time"])
        .build();
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    top.append(&marker);
    top.append(&title);
    top.append(&time);
    let source = gtk::Label::builder()
        .label(&item.feed_name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["signals-meta"])
        .build();
    let star = gtk::Label::builder()
        .label("★")
        .visible(item.is_starred)
        .css_classes(["signals-star"])
        .build();
    let metadata = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    metadata.set_margin_start(23);
    metadata.append(&source);
    metadata.append(&star);
    let row = gtk::Box::new(gtk::Orientation::Vertical, 5);
    row.append(&top);
    row.append(&metadata);
    row
}

fn refresh_article_row(view: &View, stable_id: &str) {
    let articles = view.articles.borrow();
    let Some(index) = articles
        .iter()
        .position(|article| article.stable_id == stable_id)
    else {
        return;
    };
    if let Some(row) = view.article_list.row_at_index(index as i32) {
        row.set_child(Some(&article_row_content(&articles[index])));
    }
}

fn signal_scope(
    state: Rc<Cell<ScopeState>>,
    animation_frame: Rc<Cell<u8>>,
    levels: Rc<RefCell<Vec<f64>>>,
    width: i32,
    height: i32,
) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .width_request(width)
        .height_request(height)
        .hexpand(false)
        .build();
    area.set_draw_func({
        let animation_frame = animation_frame.clone();
        let state = state.clone();
        let levels = levels.clone();
        move |_, context, width, height| {
            let width = f64::from(width);
            let height = f64::from(height);
            let left = 5.5;
            let right = width - 5.5;
            let top = 7.5;
            let bottom = height - 7.5;
            let baseline = top + (bottom - top) * 0.58;

            context.set_line_width(1.0);
            context.set_source_rgba(0.55, 0.50, 0.64, 0.16);
            for fraction in [0.25, 0.5, 0.75] {
                let y = top + (bottom - top) * fraction;
                context.move_to(left, y);
                context.line_to(right, y);
            }
            let _ = context.stroke();

            match state.get() {
                ScopeState::Flatline => draw_flatline(context, left, right, baseline),
                ScopeState::Interrupted => draw_interrupted_trace(context, left, right, baseline),
                ScopeState::Activity | ScopeState::Scanning => {
                    draw_activity_trace(context, &levels.borrow(), left, right, top, bottom);
                    if state.get() == ScopeState::Scanning {
                        let progress = f64::from(animation_frame.get().min(6)) / 6.0;
                        let x = left + (right - left) * progress;
                        context.set_source_rgba(0.78, 0.64, 1.0, 0.82);
                        context.set_line_width(1.4);
                        context.move_to(x, top - 1.0);
                        context.line_to(x, bottom + 1.0);
                        let _ = context.stroke();
                    }
                }
            }
        }
    });
    area
}

fn draw_flatline(context: &gtk::cairo::Context, left: f64, right: f64, baseline: f64) {
    context.set_source_rgba(0.61, 0.38, 0.98, 0.78);
    context.set_line_width(1.5);
    context.move_to(left, baseline);
    context.line_to(right, baseline);
    let _ = context.stroke();

    context.set_source_rgba(0.75, 0.60, 1.0, 0.40);
    for fraction in [0.2, 0.5, 0.8] {
        let x = left + (right - left) * fraction;
        context.move_to(x, baseline - 2.0);
        context.line_to(x, baseline + 2.0);
    }
    let _ = context.stroke();
}

fn draw_activity_trace(
    context: &gtk::cairo::Context,
    levels: &[f64],
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
) {
    if levels.is_empty() {
        draw_flatline(context, left, right, top + (bottom - top) * 0.58);
        return;
    }
    let baseline = top + (bottom - top) * 0.77;
    let step = (right - left) / levels.len() as f64;
    let bar_width = (step * 0.46).clamp(1.5, 3.5);
    for (index, level) in levels.iter().enumerate() {
        let amplitude = 3.0 + level * (bottom - top) * 0.66;
        let x = left + index as f64 * step + (step - bar_width) / 2.0;
        context.set_source_rgba(0.61, 0.38, 0.98, 0.45 + level * 0.48);
        context.rectangle(x, baseline - amplitude, bar_width, amplitude);
        let _ = context.fill();
    }
    context.set_source_rgba(0.75, 0.60, 1.0, 0.58);
    context.set_line_width(1.0);
    for (index, level) in levels.iter().enumerate() {
        let x = left + (index as f64 + 0.5) * step;
        let y = baseline - 3.0 - level * (bottom - top) * 0.66;
        if index == 0 {
            context.move_to(x, y);
        } else {
            context.line_to(x, y);
        }
    }
    let _ = context.stroke();
}

fn draw_interrupted_trace(context: &gtk::cairo::Context, left: f64, right: f64, baseline: f64) {
    let gap = 8.0;
    let center = (left + right) / 2.0;
    context.set_source_rgba(0.61, 0.38, 0.98, 0.72);
    context.set_line_width(1.5);
    context.move_to(left, baseline);
    context.line_to(center - gap, baseline);
    context.move_to(center + gap, baseline);
    context.line_to(right, baseline);
    context.move_to(center - 4.0, baseline - 4.0);
    context.line_to(center + 4.0, baseline + 4.0);
    context.move_to(center + 4.0, baseline - 4.0);
    context.line_to(center - 4.0, baseline + 4.0);
    let _ = context.stroke();
}

fn signal_levels(items: &[ArticleListItem], now: i64, bucket_count: usize) -> Vec<f64> {
    signal_levels_from_timestamps(
        items.iter().map(|item| item.sort_timestamp),
        now,
        bucket_count,
    )
}

fn signal_levels_from_timestamps(
    timestamps: impl IntoIterator<Item = i64>,
    now: i64,
    bucket_count: usize,
) -> Vec<f64> {
    let mut levels = vec![0.0_f64; bucket_count];
    if bucket_count == 0 {
        return levels;
    }
    const WINDOW_SECONDS: i64 = 24 * 60 * 60;
    for timestamp in timestamps {
        let age = now.saturating_sub(timestamp).clamp(0, WINDOW_SECONDS);
        let recency = WINDOW_SECONDS - age;
        let index = ((recency as usize * (bucket_count - 1)) / WINDOW_SECONDS as usize)
            .min(bucket_count - 1);
        levels[index] = (levels[index] + 0.22).min(1.0);
    }
    levels
}

fn help_overlay() -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 9);
    panel.set_halign(gtk::Align::Center);
    panel.set_valign(gtk::Align::Center);
    panel.set_width_request(420);
    panel.add_css_class("signals-help");
    for line in [
        "FEEDLIZARD // KEYS",
        "",
        "j / k       next / previous article",
        "enter       preview + mark read",
        "r           read / unread",
        "s           star / unstar",
        "o           open original",
        "shift + r   refresh feeds",
        "?           toggle this reference",
        "q / escape  back or dismiss",
    ] {
        panel.append(&gtk::Label::builder().label(line).xalign(0.0).build());
    }
    panel
}

fn relative_time(timestamp: i64) -> String {
    let seconds = (unix_now() - timestamp).max(0);
    match seconds {
        0..=59 => "NOW".into(),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn concise(value: &str, limit: usize) -> String {
    let mut result = value.chars().take(limit).collect::<String>();
    if value.chars().count() > limit {
        result.push('…');
    }
    result
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(
        r#"
        .signals-window {{ background: #09090d; color: #d8d7e2; font-family: monospace; }}
        .signals-window * {{ border-radius: 0; }}
        .signals-header {{ min-height: 70px; padding: 0 17px; background: #0d0d13; border-bottom: 1px solid #292832; }}
        .signals-brand {{ color: {PURPLE}; font-size: 13px; font-weight: 900; letter-spacing: 2.2px; }}
        .signals-metrics {{ background: #0b0b10; border-bottom: 1px solid #292832; }}
        .signals-metric {{ min-height: 60px; padding: 11px 17px 9px; border-right: 1px solid #25242d; }}
        .signals-metric-value {{ color: #efedf5; font-size: 18px; font-weight: 900; letter-spacing: .2px; font-variant-numeric: tabular-nums; }}
        .signals-metric-name {{ color: #85818e; font-size: 9px; font-weight: 800; letter-spacing: 1.2px; }}
        .signals-health .signals-metric-value {{ color: {PURPLE}; }}
        .signals-section {{ padding: 14px 17px 9px; color: #85818e; font-size: 9px; font-weight: 900; letter-spacing: 2.1px; }}
        .signals-list {{ background: transparent; }}
        .signals-row > box {{ padding: 10px 17px 11px; border-bottom: 1px solid #22212a; }}
        .signals-row:selected {{ background: #1b1825; box-shadow: inset 2px 0 {PURPLE}, inset 0 0 0 1px alpha({PURPLE}, .12); }}
        .signals-row:selected .signals-title {{ color: #f1edf8; }}
        .signals-row:selected .signals-meta {{ color: #9792a2; }}
        .signals-title {{ color: #e7e4ed; font-size: 14px; font-weight: 750; letter-spacing: -.15px; }}
        .signals-title.read {{ color: #817e89; font-weight: 500; }}
        .signals-unread {{ color: {PURPLE}; font-size: 9px; }}
        .signals-read-marker {{ color: #484650; font-size: 10px; }}
        .signals-meta, .signals-time {{ color: #817e8a; font-size: 10px; font-variant-numeric: tabular-nums; }}
        .signals-time {{ color: #aaa6b1; font-weight: 700; }}
        .signals-star {{ color: {PURPLE}; font-size: 11px; font-weight: 900; }}
        .signals-status {{ min-height: 38px; padding: 0 7px 0 15px; background: {PURPLE}; color: #0a0810; }}
        .signals-status-copy {{ font-size: 9px; font-weight: 900; letter-spacing: .45px; }}
        .signals-open-full {{ min-height: 28px; padding: 0 10px 0 14px; background: transparent; color: #0a0810; border-left: 1px solid alpha(#0a0810, .30); font-size: 9px; font-weight: 900; letter-spacing: .35px; }}
        .signals-open-full:hover {{ background: alpha(#fff, .16); }}
        .signals-empty {{ padding: 54px 20px 76px; }}
        .signals-empty-title {{ color: #aaa7b3; font-weight: 900; letter-spacing: 2px; }}
        .signals-empty-copy {{ color: #686572; font-style: italic; }}
        .signals-preview-title {{ color: #f0edf6; font-family: sans-serif; font-size: 23px; font-weight: 800; }}
        .signals-preview-body {{ color: #c5c2cc; font-family: sans-serif; font-size: 15px; line-height: 1.45; }}
        .signals-link {{ color: {PURPLE}; padding: 4px 0; }}
        .signals-flat-button {{ padding: 4px 0; background: transparent; color: #858290; font-size: 10px; font-weight: 900; }}
        .signals-help {{ padding: 25px 28px; background: #0e0d14; color: #dedbe8; border: 1px solid {PURPLE}; box-shadow: 0 16px 50px alpha(#000, .75); }}
        .signals-help label:first-child {{ color: {PURPLE}; font-weight: 900; letter-spacing: 1px; }}
    "#
    ));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_activity_is_bounded_and_places_recent_articles_on_the_right() {
        let now = 1_800_000_000;
        let levels =
            signal_levels_from_timestamps([now, now - 30, now - 3_600, now - 86_400], now, 18);
        assert_eq!(levels.len(), 18);
        assert!(levels.iter().all(|level| (0.0..=1.0).contains(level)));
        assert!(levels[17] > 0.0, "new activity belongs at the right edge");
        assert!(levels[0] > 0.0, "the full 24-hour window remains visible");
    }

    #[test]
    fn signal_activity_accumulates_without_exceeding_full_scale() {
        let now = 1_800_000_000;
        let levels = signal_levels_from_timestamps(std::iter::repeat_n(now, 20), now, 18);
        assert_eq!(levels[17], 1.0);
    }

    #[test]
    fn empty_and_zero_width_signal_activity_are_safe() {
        assert!(
            signal_levels_from_timestamps([], 1_800_000_000, 18)
                .iter()
                .all(|level| *level == 0.0)
        );
        assert!(signal_levels_from_timestamps([1_800_000_000], 1_800_000_000, 0).is_empty());
    }
}
