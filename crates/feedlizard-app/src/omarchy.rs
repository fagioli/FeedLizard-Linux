pub fn detected() -> bool {
    detected_from(
        std::env::var("OMARCHY_PATH").ok().as_deref(),
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
    )
}

fn detected_from(omarchy_path: Option<&str>, desktop: Option<&str>) -> bool {
    omarchy_path.is_some_and(|path| {
        let path = path.trim();
        !path.is_empty() && path.starts_with('/')
    }) || desktop.is_some_and(|value| {
        value
            .split([':', ';'])
            .any(|part| part.trim().eq_ignore_ascii_case("omarchy"))
    })
}

pub fn install_command() -> Option<String> {
    option_env!("FEEDLIZARD_OMARCHY_PLUGIN_REPOSITORY")
        .map(|repository| format!("omarchy plugin add {repository} --enable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_is_confident_and_non_invasive() {
        assert!(detected_from(Some("/usr/share/omarchy"), None));
        assert!(detected_from(None, Some("Hyprland:Omarchy")));
        assert!(!detected_from(None, Some("GNOME")));
        assert!(!detected_from(Some(""), None));
        assert!(!detected_from(Some("relative/path"), None));
    }
}
