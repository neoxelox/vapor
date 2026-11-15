//! Disposable sandbox layout and the environment every child process
//! inherits. A [`Home`] is one daemon's entire universe: its
//! `VAPOR_DIR` (config, logs, state, socket, lock), the local root it
//! watches, and the cloud root the filesystem provider treats as the
//! cloud side.
//!
//! `VAPOR_DIR` alone does not hold a child inside the sandbox: a path
//! that starts with `~`, the system trash, the service directory, and
//! the login keychain all hang off the user's home directory. So the
//! sandbox also points `HOME` at a directory of its own. Whatever a
//! binary under test does with `~`, an older binary with other defaults
//! included, lands in the sandbox, and the user's keychain is out of
//! reach.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use vapor_shared::constants;

use crate::Failure;

#[derive(Clone, Debug)]
pub struct Home {
    /// Short label used in diagnostics (`primary`, `pull`, `deep`).
    pub label: String,
    /// `VAPOR_DIR` for this home.
    pub dir: PathBuf,
    /// The configured local sync root (the daemon creates it if missing).
    pub local: PathBuf,
    /// The configured cloud sync root for the filesystem provider.
    pub cloud: PathBuf,
    /// The home directory (`HOME`) of every process bound to this
    /// home. `None` leaves the user's own in place.
    pub user_home: Option<PathBuf>,
    /// Extra environment for every process bound to this home.
    pub extra_env: BTreeMap<String, String>,
    /// Environment variables removed for every process bound to this
    /// home.
    pub removed_env: Vec<String>,
}

impl Home {
    pub fn state_db(&self) -> PathBuf {
        self.dir
            .join(constants::runtime::STATE_DIRECTORY_NAME)
            .join(constants::runtime::SQLITE_DATABASE_FILE_NAME)
    }

    pub fn profile_state_db(&self, profile_id: &str) -> PathBuf {
        self.dir
            .join(constants::runtime::STATE_DIRECTORY_NAME)
            .join("profiles")
            .join(profile_id)
            .join(constants::runtime::SQLITE_DATABASE_FILE_NAME)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.dir.join(constants::runtime::LOGS_DIRECTORY_NAME)
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.logs_dir()
            .join(constants::runtime::DAEMON_LOG_FILE_NAME)
    }

    pub fn socket_path(&self) -> PathBuf {
        self.dir.join(constants::ipc::SOCKET_FILE_NAME)
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join(constants::runtime::CONFIGURATION_FILE_NAME)
    }

    pub fn lifecycle_state(&self) -> PathBuf {
        self.dir
            .join(constants::runtime::STATE_DIRECTORY_NAME)
            .join(constants::runtime::LIFECYCLE_STATE_FILE_NAME)
    }

    /// Applies the sandbox environment to a command: the sandbox's own
    /// home directory, `VAPOR_DIR`, `VAPOR_ENV=dev`, debug logging,
    /// static throttle inputs, then this home's own additions and
    /// removals.
    pub fn apply_env(&self, command: &mut Command) {
        for name in &self.removed_env {
            command.env_remove(name);
        }
        if let Some(user_home) = &self.user_home {
            pin_home(command, user_home);
        }
        command
            .env(constants::env::VAPOR_DIR, &self.dir)
            .env(constants::env::VAPOR_ENV, "dev")
            .env(constants::env::VAPOR_LOG_LEVEL, "debug")
            // Pin the throttle to neutral inputs: on a developer machine
            // the host sampler would hold the daemon at Throttled while
            // the developer types, and reconcile only runs in Idle.
            .env(
                constants::env::VAPOR_THROTTLE_INPUTS,
                constants::engine::THROTTLE_INPUTS_STATIC,
            );
        for (name, value) in &self.extra_env {
            command.env(name, value);
        }
    }
}

/// The variables a process reads its home directory from: `HOME` on
/// Unix, `USERPROFILE` on Windows.
const USER_HOME_ENV: [&str; 2] = ["HOME", "USERPROFILE"];

/// The variables that move a per-user folder away from the home
/// directory. Vapor reads the first two (the system trash, the service
/// unit); with all of them unset, each folder is under the home.
const XDG_HOME_ENV: [&str; 4] = [
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
];

/// Gives `command` the home directory `home`, so no default path of the
/// process it starts reaches the real home of the user who runs the
/// harness.
pub fn pin_home(command: &mut Command, home: &Path) {
    for name in USER_HOME_ENV {
        command.env(name, home);
    }
    for name in XDG_HOME_ENV {
        command.env_remove(name);
    }
}

/// One scenario's sandbox: a directory holding one or more homes.
#[derive(Clone, Debug)]
pub struct Sandbox {
    pub root: PathBuf,
    /// The home directory its child processes get; `None` when they
    /// keep the user's.
    user_home: Option<PathBuf>,
}

impl Sandbox {
    pub fn create(root: &Path) -> Result<Self, Failure> {
        let user_home = root.join("user");
        fs::create_dir_all(&user_home)?;
        Ok(Self {
            root: root.to_path_buf(),
            user_home: Some(user_home),
        })
    }

    /// Leaves the user's home directory in place for the child
    /// processes. Two kinds of run need it: a Google Drive run reads its
    /// tokens from the login keychain, and a launchd run installs a
    /// LaunchAgent the way a real install does.
    pub fn keeping_user_home(mut self) -> Self {
        self.user_home = None;
        self
    }

    /// Provisions a home under the sandbox. `label` becomes the
    /// directory prefix (`primary` gives `home/`, `local/`, `cloud/`;
    /// any other label gives `<label>-home/` and friends) so several
    /// daemons can share one sandbox without sharing state.
    pub fn home(&self, label: &str) -> Result<Home, Failure> {
        let (dir, local, cloud) = if label == "primary" {
            (
                self.root.join("home"),
                self.root.join("local"),
                self.root.join("cloud").join("Vapor"),
            )
        } else {
            (
                self.root.join(format!("{label}-home")),
                self.root.join(format!("{label}-local")),
                self.root.join("cloud").join(format!("Vapor-{label}")),
            )
        };
        fs::create_dir_all(&dir)?;
        // The cloud parent exists; the cloud root itself is created by
        // the provider (`ensure_cloud_root`) unless a scenario
        // seeds it first, and the local root by the daemon.
        if let Some(parent) = cloud.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Home {
            label: label.to_string(),
            dir,
            local,
            cloud,
            user_home: self.user_home.clone(),
            extra_env: BTreeMap::new(),
            removed_env: Vec::new(),
        })
    }

    /// A home whose `VAPOR_DIR` is deliberately deeper than the Unix
    /// socket-address budget, to exercise the socket relocation path.
    pub fn deep_home(&self) -> Result<Home, Failure> {
        let filler = "x".repeat(70);
        let dir = self.root.join(format!("deep-{filler}")).join("home");
        fs::create_dir_all(&dir)?;
        let cloud = self.root.join("cloud").join("Vapor-deep");
        fs::create_dir_all(cloud.parent().expect("cloud parent"))?;
        Ok(Home {
            label: "deep".to_string(),
            dir,
            local: self.root.join("deep-local"),
            cloud,
            user_home: self.user_home.clone(),
            extra_env: BTreeMap::new(),
            removed_env: Vec::new(),
        })
    }
}

/// Removes a sandbox directory tree. Best effort: a file the daemon
/// still holds open on Windows is reported, never fatal.
pub fn remove_tree(path: &Path) -> Result<(), Failure> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}
