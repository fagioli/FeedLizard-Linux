mod discover_feeds;
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
const OMARCHY_APPLICATION_ID: &str = "io.github.feedlizard.FeedLizard.Omarchy";

fn application_id(mode: runtime_mode::RuntimeMode) -> &'static str {
    match mode {
        runtime_mode::RuntimeMode::Standard => APPLICATION_ID,
        runtime_mode::RuntimeMode::Omarchy => OMARCHY_APPLICATION_ID,
    }
}

fn main() -> glib::ExitCode {
    let launch = runtime_mode::Launch::from_process();
    adw::init().expect("libadwaita initialization failed");
    let application = adw::Application::builder()
        .application_id(application_id(launch.mode))
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_and_standard_have_distinct_runtime_identities() {
        assert_eq!(
            application_id(runtime_mode::RuntimeMode::Standard),
            APPLICATION_ID
        );
        assert_eq!(
            application_id(runtime_mode::RuntimeMode::Omarchy),
            OMARCHY_APPLICATION_ID
        );
        assert_ne!(APPLICATION_ID, OMARCHY_APPLICATION_ID);
    }
}
