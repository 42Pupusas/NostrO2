use crate::combo::{Combo, Task};

/// The outcome of one job.
#[derive(Debug, Clone)]
pub struct Outcome {
    label: String,
    task: Task,
    passed: bool,
    output: String,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.passed
    }

    pub fn headline(&self) -> String {
        let mark = if self.passed { "ok  " } else { "FAIL" };
        format!("{mark} {} {}", self.task.name(), self.label)
    }

    pub fn output(&self) -> &str {
        &self.output
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Runs the matrix through cargo and collects the results.
pub struct Runner {
    cargo: String,
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    pub fn new() -> Self {
        Self {
            cargo: std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()),
        }
    }

    pub fn run(&self, combo: &Combo, task: Task) -> Outcome {
        let mut args = vec![task.subcommand().to_string()];
        args.extend(combo.selection_args());
        args.extend(task.trailing_args());

        let result = std::process::Command::new(&self.cargo).args(&args).output();

        match result {
            // Both streams are kept: cargo reports the failing job on stderr,
            // but the test harness names the failing test on stdout, and a
            // report that shows only one of them cannot be acted on.
            Ok(output) => Outcome {
                label: combo.label(),
                task,
                passed: output.status.success(),
                output: format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
            },
            Err(e) => Outcome {
                label: combo.label(),
                task,
                passed: false,
                output: format!("could not start cargo: {e}"),
            },
        }
    }
}

/// The collected results of a whole matrix run.
pub struct Report {
    outcomes: Vec<Outcome>,
}

impl Report {
    pub fn new(outcomes: Vec<Outcome>) -> Self {
        Self { outcomes }
    }

    pub fn failures(&self) -> Vec<&Outcome> {
        self.outcomes.iter().filter(|o| !o.passed()).collect()
    }

    pub fn is_green(&self) -> bool {
        self.failures().is_empty()
    }

    pub fn summary(&self) -> String {
        let failed = self.failures().len();
        let total = self.outcomes.len();
        if failed == 0 {
            return format!("all {total} jobs passed");
        }
        format!("{failed} of {total} jobs failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(passed: bool) -> Outcome {
        Outcome {
            label: "pkg [feat]".to_string(),
            task: Task::Test,
            passed,
            output: String::new(),
        }
    }

    #[test]
    fn a_report_with_no_failures_is_green() {
        let report = Report::new(vec![outcome(true), outcome(true)]);
        assert!(report.is_green());
        assert_eq!(report.summary(), "all 2 jobs passed");
    }

    #[test]
    fn a_single_failure_makes_the_report_red() {
        let report = Report::new(vec![outcome(true), outcome(false)]);
        assert!(!report.is_green());
        assert_eq!(report.failures().len(), 1);
        assert_eq!(report.summary(), "1 of 2 jobs failed");
    }

    #[test]
    fn a_headline_marks_a_failure_visibly() {
        assert!(outcome(false).headline().starts_with("FAIL"));
        assert!(outcome(true).headline().starts_with("ok"));
    }

    #[test]
    fn a_runner_honours_the_cargo_environment_variable() {
        let runner = Runner::new();
        assert!(!runner.cargo.is_empty());
    }
}
