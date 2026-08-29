#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeMode {
    Standard,
    Omarchy,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Launch {
    pub mode: RuntimeMode,
    pub gtk_args: Vec<String>,
}

impl Launch {
    pub fn from_process() -> Self {
        Self::from_inputs(std::env::args(), std::env::var("FEEDLIZARD_OMARCHY").ok())
    }

    fn from_inputs(args: impl IntoIterator<Item = String>, environment: Option<String>) -> Self {
        let mut explicit = false;
        let gtk_args = args
            .into_iter()
            .filter(|argument| {
                if argument == "--omarchy" {
                    explicit = true;
                    false
                } else {
                    true
                }
            })
            .collect();
        let mode = if explicit || environment.as_deref() == Some("1") {
            RuntimeMode::Omarchy
        } else {
            RuntimeMode::Standard
        };
        Self { mode, gtk_args }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(arguments: &[&str], environment: Option<&str>) -> Launch {
        Launch::from_inputs(
            arguments.iter().map(|value| (*value).to_owned()),
            environment.map(str::to_owned),
        )
    }

    #[test]
    fn ordinary_launch_is_standard() {
        assert_eq!(launch(&["feedlizard"], None).mode, RuntimeMode::Standard);
    }

    #[test]
    fn explicit_argument_selects_omarchy_and_is_not_forwarded_to_gtk() {
        let result = launch(&["feedlizard", "--omarchy"], None);
        assert_eq!(result.mode, RuntimeMode::Omarchy);
        assert_eq!(result.gtk_args, ["feedlizard"]);
    }

    #[test]
    fn environment_is_an_exact_fallback() {
        assert_eq!(
            launch(&["feedlizard"], Some("1")).mode,
            RuntimeMode::Omarchy
        );
        assert_eq!(
            launch(&["feedlizard"], Some("true")).mode,
            RuntimeMode::Standard
        );
    }

    #[test]
    fn explicit_argument_has_priority_over_environment() {
        assert_eq!(
            launch(&["feedlizard", "--omarchy"], Some("0")).mode,
            RuntimeMode::Omarchy
        );
    }
}
