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

        println!("running {} jobs\n", jobs.len());

        let mut outcomes = Vec::with_capacity(jobs.len());
        for (combo, task) in jobs {
            let outcome = runner.run(&combo, task);
            println!("{}", outcome.headline());
            outcomes.push(outcome);
        }

        let report = runner::Report::new(outcomes);
        println!("\n{}", report.summary());

        for failure in report.failures() {
            println!("\n=== {} ===\n{}", failure.label(), failure.output());
        }

        if report.is_green() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        }
    }
}

fn main() -> std::process::ExitCode {
    Cli::main()
}
