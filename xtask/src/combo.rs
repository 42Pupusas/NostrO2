/// One feature combination of one package.
///
/// A combination names every feature explicitly and never relies on the
/// defaults, because the defaults are exactly what hides a broken
/// combination from a plain `cargo test`.
#[derive(Debug, Clone)]
pub struct Combo {
    package: String,
    features: Vec<String>,
}

impl Combo {
    pub fn new(package: &str, features: &[&str]) -> Self {
        Self {
            package: package.to_string(),
            features: features.iter().map(|f| (*f).to_string()).collect(),
        }
    }

    #[cfg(test)]
    pub fn package(&self) -> &str {
        &self.package
    }

    pub fn feature_list(&self) -> String {
        self.features.join(",")
    }

    pub fn label(&self) -> String {
        format!("{} [{}]", self.package, self.feature_list())
    }

    /// The cargo arguments that select this combination.
    pub fn selection_args(&self) -> Vec<String> {
        vec![
            "-p".to_string(),
            self.package.clone(),
            "--no-default-features".to_string(),
            "--features".to_string(),
            self.feature_list(),
        ]
    }
}

/// What to run against a combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Test,
    Clippy,
}

impl Task {
    pub const fn subcommand(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Clippy => "clippy",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Clippy => "clippy",
        }
    }

    /// Arguments appended after the feature selection.
    pub fn trailing_args(self) -> Vec<String> {
        match self {
            Self::Test => Vec::new(),
            Self::Clippy => vec![
                "--all-targets".to_string(),
                "--".to_string(),
                "-D".to_string(),
                "warnings".to_string(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_combo_never_relies_on_default_features() {
        let combo = Combo::new("nostro2-relay", &["serde", "k256"]);
        assert!(
            combo
                .selection_args()
                .contains(&"--no-default-features".to_string()),
            "a matrix that inherits defaults cannot prove a non-default combination"
        );
    }

    #[test]
    fn a_combo_joins_its_features_with_commas() {
        let combo = Combo::new("nostro2-relay", &["rustls-ring", "serde", "k256"]);
        assert_eq!(combo.feature_list(), "rustls-ring,serde,k256");
    }

    #[test]
    fn a_label_names_the_package_and_the_features() {
        let combo = Combo::new("nostro2", &["serde"]);
        assert_eq!(combo.label(), "nostro2 [serde]");
    }

    #[test]
    fn clippy_denies_warnings() {
        let args = Task::Clippy.trailing_args();
        assert!(args.contains(&"-D".to_string()) && args.contains(&"warnings".to_string()));
    }

    #[test]
    fn test_takes_no_trailing_arguments() {
        assert!(Task::Test.trailing_args().is_empty());
    }
}
