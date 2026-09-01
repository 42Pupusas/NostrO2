//! Workspace verification matrix.
//!
//! The curve and JSON backends are cargo features, and cargo unifies
//! features across a package's normal and dev dependency graphs. A
//! dev-dependency that pins one backend therefore makes the other backend
//! impossible to test, and the untested one breaks silently. This tool
//! builds, lints, and tests every supported combination so that cannot
//! happen again.
//!
//! Run it with `cargo run -p xtask`.

mod combo;
mod matrix;
mod runner;

/// The command line entry point.
struct Cli;

impl Cli {
    fn main() -> std::process::ExitCode {
        let matrix = matrix::Matrix::new();
        let runner = runner::Runner::new();
        let jobs = matrix.jobs();

        let total = jobs.len();
        println!("running {total} jobs\n");

        // Each job rebuilds from scratch, because every combination changes
        // the feature set. Announce the job before running it: a run that
        // prints nothing for minutes is indistinguishable from a hang.
        let started = std::time::Instant::now();
        let mut outcomes = Vec::with_capacity(total);
        for (index, (combo, task)) in jobs.into_iter().enumerate() {
            print!("[{:>2}/{total}] {} {} ... ", index + 1, task.name(), combo.label());
            Self::flush();

            let job_started = std::time::Instant::now();
            let outcome = runner.run(&combo, task);
            println!(
                "{} ({:.1}s)",
                if outcome.passed() { "ok" } else { "FAIL" },
                job_started.elapsed().as_secs_f64()
            );
            outcomes.push(outcome);
        }

        let report = runner::Report::new(outcomes);
        println!(
            "\n{} in {:.1}s",
            report.summary(),
            started.elapsed().as_secs_f64()
        );

        for failure in report.failures() {
            println!("\n=== {} ===\n{}", failure.label(), failure.output());
        }

        if report.is_green() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        }
    }

    /// Pushes the pending line out before a job starts, so the label is
    /// visible while the job runs rather than after it ends.
    fn flush() {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }
}

fn main() -> std::process::ExitCode {
    Cli::main()
}
