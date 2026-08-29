mod image_worker;
mod network_worker;
mod nostr_worker;
mod omarchy;
mod omarchy_ui;
mod runtime_mode;
mod ui;
mod worker;

use adw::prelude::*;
use gtk::{gio, glib};

const APPLICATION_ID: &str = "io.github.feedlizard.FeedLizard";

fn main() -> glib::ExitCode {
    let launch = runtime_mode::Launch::from_process();
    adw::init().expect("libadwaita initialization failed");
    let flags = match launch.mode {
        runtime_mode::RuntimeMode::Standard => gio::ApplicationFlags::HANDLES_OPEN,
        runtime_mode::RuntimeMode::Omarchy => {
            gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE
        }
    };
    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .flags(flags)
        .build();
    match launch.mode {
        runtime_mode::RuntimeMode::Standard => {
            application.connect_startup(ui::install_actions);
            application.connect_activate(ui::build_window)
        }
        runtime_mode::RuntimeMode::Omarchy => {
            application.connect_startup(omarchy_ui::install_actions);
            application.connect_activate(omarchy_ui::build_window)
        }
    };
    application.run_with_args(&launch.gtk_args)
}
