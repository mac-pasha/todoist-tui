//! TOML config loading (T-005, EPIC-01).
//!
//! [`Config::load`] resolves the platform config file through
//! [`paths::config_file`](crate::config::paths::config_file) at call time and
//! delegates to the pure [`Config::load_from`] seam, which reads and parses
//! exactly one file and nothing else — most importantly it never writes, so a
//! missing config file stays missing and the app starts on defaults. Parse
//! failures map to `ConfigError::Parse` with 1-based line/column recovered
//! from the `toml` parser's byte span.

// Same ahead-of-consumer situation as model.rs: `Config::load` has no caller
// until a later task wires the binary, and `pub` alone does not silence
// `dead_code` in a binary crate. Remove once a consumer exists.
#![allow(dead_code)]

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::config::model::Config;
use crate::config::paths;
use crate::error::ConfigError;

impl Config {
    /// Loads the configuration from the platform config file.
    ///
    /// The path is resolved at call time: setting `XDG_CONFIG_HOME` (or
    /// changing `HOME`) before calling changes the result, which is what makes
    /// the loader testable without globals or caching (same contract as
    /// [`paths`]).
    ///
    /// A missing file is not an error: the returned [`Config`] is
    /// [`Config::default`] and the file is **not** created — a TUI must never
    /// conjure config the user never wrote.
    ///
    /// # Errors
    ///
    /// - `ConfigError::Io` — the file exists but cannot be read.
    /// - `ConfigError::Parse` — the file is not valid TOML, carries an unknown
    ///   key, or holds a value of the wrong shape.
    pub fn load() -> Result<Self, ConfigError> {
        let path = paths::config_file()?;
        Self::load_from(&path)
    }

    /// Loads the configuration from an explicit `path`.
    ///
    /// Pure test seam behind [`Config::load`]: path resolution happens in the
    /// caller, so tests can point this at a tempdir `config.toml` without
    /// touching the process environment.
    ///
    /// A missing file (or a missing parent directory) yields
    /// [`Config::default`], exactly like [`Config::load`]; the file is never
    /// created.
    ///
    /// # Errors
    ///
    /// Same as [`Config::load`].
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            // Missing file *or* missing parent directory: both surface as
            // NotFound and both mean "no config yet" -> defaults.
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str(&source).map_err(|error| parse_error(path, &source, &error))
    }
}

/// Maps a `toml` parse/serde error onto `ConfigError::Parse`.
///
/// The `toml` crate reports a byte span into the source (the parser yields
/// token boundaries, so span starts are char boundaries); converting it here
/// keeps the `ConfigError::Parse` 1-based position contract without leaking
/// parser internals. When a serde-level error arrives without a span, the
/// position falls back to 1:1 (the top of the file) rather than a
/// meaningless 0.
fn parse_error(path: &Path, source: &str, error: &toml::de::Error) -> ConfigError {
    let (line, column) = error
        .span()
        .map_or((1, 1), |span| line_col(source, span.start));
    ConfigError::Parse {
        path: path.to_path_buf(),
        line,
        column,
        message: error.message().to_owned(),
    }
}

/// Converts a byte offset in `source` to a 1-based `(line, column)` pair —
/// line is the newline count before the offset plus one, column the number of
/// chars since the last newline plus one, the same arithmetic the `toml` crate
/// uses when rendering its own errors.
///
/// Rounds a non-boundary offset down to the nearest char boundary first, so
/// the function stays panic-free for arbitrary input even though the parser
/// never produces such offsets.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|b| *b == b'\n').count();
    let column = prefix.chars().rev().take_while(|c| *c != '\n').count();
    (line + 1, column + 1)
}
