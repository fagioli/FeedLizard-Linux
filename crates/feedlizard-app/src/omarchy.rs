pub fn detected() -> bool {
    detected_from(std::env::var("OMARCHY_PATH").ok().as_deref())
}

fn detected_from(omarchy_path: Option<&str>) -> bool {
    omarchy_path.is_some_and(|path| path.trim().trim_end_matches('/') == "/usr/share/omarchy")
}

pub fn install_command() -> Option<String> {
    option_env!("FEEDLIZARD_OMARCHY_PLUGIN_REPOSITORY")
        .map(|repository| format!("omarchy plugin add {repository} --enable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_requires_the_production_omarchy_marker() {
        assert!(detected_from(Some("/usr/share/omarchy")));
        assert!(detected_from(Some(" /usr/share/omarchy/ ")));
        assert!(!detected_from(None));
        assert!(!detected_from(Some("")));
        assert!(!detected_from(Some("/tmp/omarchy")));
        assert!(!detected_from(Some("relative/path")));
    }
}
