//! Crate-wide typed error hierarchy (T-003, EPIC-01).
//!
//! Pure types only: no IO, no domain logic, no secret material. `thiserror`
//! gives library-style errors with `Display` and `source` chaining; `anyhow`
//! stays at the top of `main` in later tasks. Downstream epics extend the
//! top-level enum (EPIC-02 adds `ApiError`, EPIC-03 adds `StoreError`), so
//! `AppError` is `#[non_exhaustive]`.
//!
//! Display contract for file-touching variants (T-003 acceptance): every
//! variant that identifies an offending file renders that file's path, and
//! no variant ever interpolates a secret value (API token, credentials).

// T-003 ships the error vocabulary ahead of its consumers: T-004/T-005
// (config), T-006 (log) and T-007 (CLI) construct these types. In a binary
// crate `pub` alone does not silence `dead_code`, and main.rs stays a
// placeholder here, so allow it at module scope until those tasks land
// (first consumer: T-004 paths::config_file).
#![allow(dead_code)]

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Crate-wide result alias: every fallible signature returns this type.
pub type Result<T> = std::result::Result<T, AppError>;

/// Top-level application error.
///
/// Wraps the per-subsystem error enums; later epics append further variants.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AppError {
    /// Configuration could not be read or parsed.
    #[error("config: {0}")]
    Config(#[from] ConfigError),

    /// The logging subsystem could not be initialised.
    #[error("logging: {0}")]
    Log(#[from] LogError),

    /// A CLI subcommand is stubbed out; carries the subcommand name
    /// (T-007 maps this to a stderr message and exit code 1).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    /// Authentication / API token handling failed.
    #[error("auth: {0}")]
    Auth(#[from] AuthError),

    /// A call to the Todoist API (task fetch/sync, EPIC-03) failed.
    #[error("api: {0}")]
    Api(#[from] ApiError),

    /// The local SQLite store (open, migrate, read/write, EPIC-04) failed.
    #[error("db: {0}")]
    Db(#[from] DbError),
}

/// Errors raised while loading the TOML config file.
///
/// Every variant carries the config file's [`PathBuf`] so the user can act
/// on the exact file at fault.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The config file exists but is not valid TOML / not valid config.
    ///
    /// `line` and `column` are 1-based, mirroring `toml`'s error spans
    /// (mapped in by EPIC-01 T-005).
    #[error("failed to parse config file {path} at line {line}, column {column}: {message}")]
    Parse {
        /// Config file that failed to parse.
        path: PathBuf,
        /// 1-based line number of the offending input.
        line: usize,
        /// 1-based column number of the offending input.
        column: usize,
        /// Parser message; carries positions only, never file contents.
        message: String,
    },

    /// Reading the config file failed at the OS level.
    #[error("config file I/O failed for {path}: {source}")]
    Io {
        /// Config file that could not be read.
        path: PathBuf,
        /// Underlying OS error.
        source: io::Error,
    },
}

/// Errors raised while authenticating against the Todoist API / handling
/// the stored API token (EPIC-02).
///
/// No variant here may carry raw token material: every field is either unit,
/// a [`PathBuf`], an [`io::Error`], or an opaque backend error. The token
/// itself only ever lives inside [`crate::auth::Token`] (a
/// `crate::log::Secret<String>`), which this enum never wraps.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AuthError {
    /// No token is available (not stored yet, or removed).
    #[error("not authenticated: no API token is stored; run `todoist-tui auth` to add one")]
    NotAuthenticated,

    /// The stored or supplied token was rejected (malformed, or the API
    /// reported it as invalid/expired).
    #[error("the API token is invalid")]
    InvalidToken,

    /// The OS credential store (keyring) failed.
    #[error("keyring backend failed: {source}")]
    Backend {
        /// Underlying keyring/OS error. Never constructed from token bytes.
        #[source]
        source: keyring::Error,
    },

    /// Reading/writing a token-related file failed at the OS level.
    #[error("token file I/O failed for {path}: {source}")]
    Io {
        /// File that could not be read/written.
        path: PathBuf,
        /// Underlying OS error.
        source: io::Error,
    },

    /// A token was supplied that is empty or whitespace-only.
    #[error("the API token must not be empty")]
    EmptyToken,

    /// A non-HTTPS base URL was supplied to the token-validation client
    /// (T-106). Never constructed from user input containing the token.
    #[error("refusing to validate the API token over a non-HTTPS URL")]
    InsecureBaseUrl,

    /// Building the HTTPS client, or sending the validation request,
    /// failed at the transport level (DNS, TLS, timeout, connection
    /// refused, ...). Carries the underlying `reqwest::Error`, which never
    /// contains the token: it is sent via an `Authorization` header, never
    /// interpolated into the request URL.
    #[error("request to the Todoist API failed: {source}")]
    Request {
        /// Underlying HTTP client/transport error.
        #[source]
        source: reqwest::Error,
    },

    /// The Todoist API responded with a status this client does not
    /// recognise as either "token accepted" (200) or "token rejected"
    /// (401/403).
    #[error("unexpected response from the Todoist API: HTTP {status}")]
    UnexpectedStatus {
        /// Raw HTTP status code returned by the API.
        status: u16,
    },

    /// [`crate::auth::validate::check_blocking`] could not build the
    /// current-thread Tokio runtime it uses to drive the async
    /// [`crate::auth::validate::check`] call from sync code.
    #[error("failed to start the async runtime for token validation: {source}")]
    Runtime {
        /// Underlying Tokio runtime-build error.
        #[source]
        source: io::Error,
    },
}

/// Errors raised while calling the Todoist REST API (EPIC-03).
///
/// No variant here may carry raw token material: the token is sent via an
/// `Authorization` header, never interpolated into a message or URL, and
/// every field below is either unit, a status/attempt count, a
/// `std::time::Duration`, or an opaque transport/decode error.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ApiError {
    /// The API rejected the request as unauthenticated/unauthorized (401).
    #[error("the API token is unauthorized")]
    Unauthorized,

    /// The API rate-limited the request (429). Carries the `Retry-After`
    /// delay when the server supplied one.
    #[error("rate limited by the Todoist API")]
    RateLimited {
        /// Delay the server asked the client to wait before retrying, if any.
        retry_after: Option<std::time::Duration>,
    },

    /// The API responded with a server-side error status (5xx).
    #[error("the Todoist API returned a server error: HTTP {status}")]
    Server {
        /// Raw HTTP status code returned by the API.
        status: u16,
    },

    /// Sending the request, or the underlying connection, failed at the
    /// transport level (DNS, TLS, timeout, connection refused, ...).
    #[error("request to the Todoist API failed: {source}")]
    Transport {
        /// Underlying HTTP client/transport error.
        #[source]
        source: reqwest::Error,
    },

    /// The response body could not be decoded into the expected shape.
    #[error("failed to decode the Todoist API response: {source}")]
    Decode {
        /// Underlying (de)serialisation error.
        #[source]
        source: reqwest::Error,
    },

    /// A non-HTTPS base URL was supplied to the API client. Never
    /// constructed from user input containing the token.
    #[error("refusing to call the Todoist API over a non-HTTPS URL")]
    InsecureBaseUrl,

    /// A request was retried until the client's retry budget was exhausted.
    #[error("request to the Todoist API failed after {attempts} attempt(s)")]
    RetriesExhausted {
        /// Total number of attempts made, including the first.
        attempts: u32,
    },
}

impl ApiError {
    /// Whether a fresh attempt of the same request might succeed: true for
    /// transient failures (`Server`, `RateLimited`, `Transport`), false for
    /// errors that will not change on retry (`Unauthorized`,
    /// `InsecureBaseUrl`, `RetriesExhausted`, `Decode`).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ApiError::Server { .. } | ApiError::RateLimited { .. } | ApiError::Transport { .. }
        )
    }
}

/// Errors raised while initialising the logging subsystem.
///
/// Kept minimal here; T-006 extends as the subscriber wiring lands.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum LogError {
    /// The state directory that should hold the log file could not be created.
    #[error("failed to create log directory {path}: {source}")]
    CreateDir {
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying OS error.
        source: io::Error,
    },

    /// The log file could not be opened for appending.
    #[error("failed to open log file {path}: {source}")]
    OpenFile {
        /// Log file that could not be opened.
        path: PathBuf,
        /// Underlying OS error.
        source: io::Error,
    },
}

/// Errors raised by the local SQLite store (EPIC-04).
///
/// No variant here may carry raw secret material: paths and schema version
/// numbers only, plus opaque `rusqlite`/decode source errors.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum DbError {
    /// The SQLite database file could not be opened (or created).
    #[error("failed to open database {path}: {source}")]
    Open {
        /// Database file that could not be opened.
        path: PathBuf,
        /// Underlying `rusqlite` error.
        #[source]
        source: rusqlite::Error,
    },

    /// A schema migration step failed partway through.
    #[error("failed to migrate database schema from version {from} to {to}: {source}")]
    Migrate {
        /// Schema version migrated from.
        from: i32,
        /// Schema version migration was attempting to reach.
        to: i32,
        /// Underlying `rusqlite` error.
        #[source]
        source: rusqlite::Error,
    },

    /// The database's on-disk schema version is newer (or otherwise
    /// incompatible) than what this build of `todoist-tui` supports.
    #[error("unsupported database schema version {found} (this build supports up to {supported})")]
    UnsupportedSchemaVersion {
        /// Schema version found on disk.
        found: i32,
        /// Highest schema version this build knows how to read/migrate.
        supported: i32,
    },

    /// A SQLite query or statement failed outside of open/migrate.
    #[error("database query failed: {source}")]
    Sqlite {
        /// Underlying `rusqlite` error.
        #[source]
        source: rusqlite::Error,
    },

    /// A stored row's payload (e.g. JSON) could not be decoded into the
    /// expected shape.
    #[error("failed to decode database row: {source}")]
    Decode {
        /// Underlying (de)serialisation error.
        #[source]
        source: serde_json::Error,
    },

    /// The database file (or its parent directory) does not have the
    /// permissions required to read/write it.
    #[error("insufficient permissions for database file {path}")]
    Permissions {
        /// Database file with the permission problem.
        path: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture token used only to assert it never leaks through error
    /// rendering; never a real credential.
    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    fn sample_db_errors() -> Vec<DbError> {
        vec![
            DbError::Open {
                path: PathBuf::from("/tmp/todoist-tui/db.sqlite"),
                source: rusqlite::Error::InvalidQuery,
            },
            DbError::Migrate {
                from: 1,
                to: 2,
                source: rusqlite::Error::InvalidQuery,
            },
            DbError::UnsupportedSchemaVersion {
                found: 99,
                supported: 2,
            },
            DbError::Sqlite {
                source: rusqlite::Error::InvalidQuery,
            },
            DbError::Decode {
                source: serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
            },
            DbError::Permissions {
                path: PathBuf::from("/tmp/todoist-tui/db.sqlite"),
            },
        ]
    }

    #[test]
    fn db_error_variants_never_leak_the_fixture_token() {
        for err in sample_db_errors() {
            let display = format!("{err}");
            let debug = format!("{err:?}");
            assert!(
                !display.contains(FIXTURE_TOKEN),
                "Display leaked fixture token: {display}"
            );
            assert!(
                !debug.contains(FIXTURE_TOKEN),
                "Debug leaked fixture token: {debug}"
            );
        }
    }

    #[test]
    fn app_error_db_has_db_prefix_and_from_conversion() {
        let app: AppError = DbError::UnsupportedSchemaVersion {
            found: 9,
            supported: 2,
        }
        .into();
        assert!(matches!(app, AppError::Db(_)));
        let shown = app.to_string();
        assert!(shown.starts_with("db: "), "got {shown}");
        assert!(shown.contains("version 9"), "got {shown}");
        assert!(shown.contains("up to 2"), "got {shown}");
    }

    #[test]
    fn app_error_db_prefix_holds_for_every_variant_without_token() {
        for err in sample_db_errors() {
            let inner = err.to_string();
            let app = AppError::from(err);
            assert_eq!(app.to_string(), format!("db: {inner}"));
            assert!(!app.to_string().contains(FIXTURE_TOKEN));
            assert!(!format!("{app:?}").contains(FIXTURE_TOKEN));
        }
    }

    #[test]
    fn db_error_display_includes_expected_fields() {
        let open = DbError::Open {
            path: PathBuf::from("/tmp/x/db.sqlite"),
            source: rusqlite::Error::InvalidQuery,
        }
        .to_string();
        assert!(open.contains("/tmp/x/db.sqlite"));
        let mig = DbError::Migrate {
            from: 3,
            to: 4,
            source: rusqlite::Error::InvalidQuery,
        }
        .to_string();
        assert!(mig.contains('3') && mig.contains('4'));
        let perm = DbError::Permissions {
            path: PathBuf::from("/tmp/x/db.sqlite"),
        }
        .to_string();
        assert!(perm.contains("/tmp/x/db.sqlite"));
        let dec = DbError::Decode {
            source: serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
        }
        .to_string();
        assert!(dec.starts_with("failed to decode"));
    }

    #[test]
    fn db_error_source_chain_exposes_underlying_error() {
        use std::error::Error as _;
        let errs = sample_db_errors();
        for err in &errs {
            match err {
                DbError::UnsupportedSchemaVersion { .. } | DbError::Permissions { .. } => {
                    assert!(err.source().is_none());
                }
                _ => assert!(err.source().is_some(), "missing source for {err:?}"),
            }
        }
    }
}
