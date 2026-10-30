//! [`TestHome`]: an isolated HOME/XDG sandbox for the real binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// The fake API token written to the sandbox's token file. Obviously fake.
pub const FAKE_TOKEN: &str = "tok_fake_abc123";

/// A temp directory used as HOME and every XDG/AppData root, containing a
/// file-backed [`FAKE_TOKEN`] where the binary's file-token fallback reads
/// it (`<root>/cfg/todoist-tui/token`). The directory is removed on drop.
pub struct TestHome {
    dir: TempDir,
    base_url: String,
}

impl TestHome {
    /// Creates the sandbox pointing the binary at `base_url` (the mock
    /// server) and writes the fake token file (mode `0600` on Unix).
    pub fn new(base_url: &str) -> Self {
        let home = Self::without_token(base_url);
        let token = home.token_file();
        fs::create_dir_all(token.parent().expect("token file has a parent"))
            .expect("create token dir");
        fs::write(&token, FAKE_TOKEN).expect("write fake token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&token, fs::Permissions::from_mode(0o600))
                .expect("chmod token file");
        }
        home
    }

    /// Like [`TestHome::new`] but with no token stored (first-run state).
    pub fn without_token(base_url: &str) -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("todoist-tui-it-")
                .tempdir()
                .expect("create temp home"),
            base_url: base_url.to_string(),
        }
    }

    /// The sandbox root (HOME).
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// `<root>/cfg/todoist-tui/token`.
    pub fn token_file(&self) -> PathBuf {
        self.root().join("cfg").join("todoist-tui").join("token")
    }

    /// `<root>/state/todoist-tui` (log files).
    pub fn state_dir(&self) -> PathBuf {
        self.root().join("state").join("todoist-tui")
    }

    /// `<root>/data` (XDG data home; cache DB lives under it).
    pub fn data_dir(&self) -> PathBuf {
        self.root().join("data")
    }

    /// The mock base URL the binary is pointed at.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// A `Command` for the real binary with every directory redirected into
    /// the sandbox, the file-token fallback forced, `TODOIST_API_BASE_URL`
    /// set to the mock, and ambient `RUST_*` variables scrubbed.
    pub fn command(&self) -> Command {
        let root = self.root();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_todoist-tui"));
        cmd.env("HOME", root)
            .env("XDG_CONFIG_HOME", root.join("cfg"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_DATA_HOME", self.data_dir())
            .env("APPDATA", root.join("appdata"))
            .env("LOCALAPPDATA", root.join("localappdata"))
            .env("TODOIST_TUI_FORCE_FILE_TOKEN", "1")
            .env("TODOIST_API_BASE_URL", &self.base_url)
            .env_remove("RUST_LOG")
            .env_remove("RUST_BACKTRACE")
            .env_remove("RUST_LIB_BACKTRACE");
        cmd
    }
}
