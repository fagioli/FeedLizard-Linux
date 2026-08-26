mod image_worker;
mod network_worker;
mod nostr_worker;
mod omarchy;
mod ui;
mod worker;

use adw::prelude::*;
use gtk::{gio, glib};

const APPLICATION_ID: &str = "io.github.feedlizard.FeedLizard";

fn main() -> glib::ExitCode {
    adw::init().expect("libadwaita initialization failed");
    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    application.connect_startup(ui::install_actions);
    application.connect_activate(ui::build_window);
    application.run()
}
