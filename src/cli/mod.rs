//! Clap CLI skeleton (T-007, EPIC-01).
//!
//! Defines the `todoist-tui` command surface: a global `--config` override
//! plus five subcommands (`auth`, `logout`, `sync`, `add`, `today`). Every
//! subcommand is a stub in this task — [`commands::dispatch`] returns
//! [`crate::error::AppError::NotImplemented`] for each of them, and so does
//! [`run`] itself when no subcommand is given (the default TUI path, not
//! built yet). `main` maps that error to a stderr message and exit code 1.
//!
//! [`build_cli`] exposes the underlying [`clap::Command`] (via
//! [`clap::CommandFactory`]) as a plain function so CLI tests (trycmd /
//! `assert_cmd`) can assert on `--help` / `--version` output without
//! spawning the binary, and so [`clap::Command::debug_assert`] has a
//! reachable call site.

pub mod commands;

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::error::{AppError, Result};

/// `todoist-tui` command-line interface.
///
/// With no subcommand, `todoist-tui` launches the TUI (default path); every
/// subcommand below instead runs a one-shot action and exits.
#[derive(Debug, Parser)]
#[command(
    name = "todoist-tui",
    version,
    about = "Terminal UI client for Todoist",
    propagate_version = true
)]
pub struct Cli {
    /// Overrides the config file location (default: the platform config
    /// directory resolved by `config::paths`).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Subcommand to run; omitted means "launch the TUI".
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// The five subcommands making up the quick-action surface; the TUI itself
/// is the no-subcommand default and is not a variant here.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Store a Todoist API token in the OS keychain.
    Auth,

    /// Remove the stored API token from the OS keychain.
    Logout,

    /// Sync tasks and projects with Todoist.
    Sync,

    /// Quick-add a task from a single line of text.
    Add {
        /// Quick-add text, parsed later (REQ-001 D1, EPIC-11); this task
        /// only captures it, it is never interpreted here.
        text: String,
    },

    /// Show tasks due today.
    Today {
        /// Print the result as JSON instead of formatted text.
        #[arg(long)]
        json: bool,
    },
}

/// Builds the underlying [`clap::Command`] for `todoist-tui`.
///
/// Exists as a plain function (rather than requiring callers to construct a
/// [`Cli`]) so CLI-surface tests can inspect `--help` / `--version` output,
/// `Command::debug_assert` can validate the derived command, and `main` has
/// a single source of truth for the parsed [`Cli`] (see [`parse`]).
#[must_use]
pub fn build_cli() -> clap::Command {
    Cli::command()
}

/// Parses process arguments into a [`Cli`] via [`build_cli`], exiting the
/// process with clap's usual help/error/exit-code-2 behaviour on failure.
///
/// This is the real entry point `main` calls; it exists so `build_cli`'s
/// `Command` is what actually gets parsed, not a second, independently
/// derived one from `Cli::parse()`.
#[must_use]
pub fn parse() -> Cli {
    let matches = build_cli().get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
}

/// Dispatches a parsed [`Cli`] to its subcommand stub.
///
/// No subcommand selected routes to the default TUI path, which is itself
/// not implemented yet in this task.
///
/// # Errors
///
/// Always returns [`AppError::NotImplemented`] in this task: `"tui"` for the
/// default path, or the subcommand's name for each stub (see
/// [`commands::dispatch`]).
pub fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        None => Err(AppError::NotImplemented("tui")),
        Some(command) => commands::dispatch(command),
    }
}

/// Sanity check that the derived [`clap::Command`] is internally consistent
/// (no duplicate args/ids, valid help text, etc.). Substantive CLI-surface
/// assertions (full `--help` text, `--version`, per-subcommand args) are the
/// unit-tester's job; this is only a reachable `debug_assert` call site.
#[cfg(test)]
mod tests {
    use super::build_cli;

    #[test]
    fn cli_definition_is_internally_consistent() {
        build_cli().debug_assert();
    }
}
