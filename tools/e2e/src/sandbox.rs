//! Disposable sandbox layout and the environment every child process
//! inherits. A [`Home`] is one daemon's entire universe: its
//! `VAPOR_DIR` (config, logs, state, socket, lock), the local root it
//! watches, and the cloud root the filesystem provider treats as the
//! cloud side.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use vapor_shared::constants;

use crate::Failure;

/// Environment variables the sandbox never lets a child inherit from
/// the invoking shell: config must flow through `vapor config set` so
/// the run covers the `vapor.json` loader.
pub const SCRUBBED_ENV: &[&str] = &[
    constants::env::VAPOR_LOCAL_SYNC_DIRECTORY,
    constants::env::VAPOR_CLOUD_SYNC_DIRECTORY,
    constants::env::VAPOR_USE_GITIGNORE,
    constants::env::VAPOR_USE_VAPORIGNORE,
    constants::env::VAPOR_PRE_IGNORE_RULES,
    constants::env::VAPOR_POST_IGNORE_RULES,
];

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
    /// Extra environment for every process bound to this home.
    pub extra_env: BTreeMap<String, String>,
    /// Environment variables removed for every process bound to this
    /// home, on top of [`SCRUBBED_ENV`].
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

    /// Applies the sandbox environment to a command: `VAPOR_DIR`,
    /// `VAPOR_ENV=dev`, debug logging, static throttle inputs, the
    /// scrub list, then this home's own additions and removals.
    pub fn apply_env(&self, command: &mut Command) {
        for name in SCRUBBED_ENV {
            command.env_remove(name);
        }
        for name in &self.removed_env {
            command.env_remove(name);
        }
        command
            .env(constants::env::VAPOR_DIR, &self.dir)
            .env(constants::env::VAPOR_ENV, "dev")
            .env(constants::env::VAPOR_LOG_LEVEL, "debug")
            // Pin the throttle to neutral inputs: on a developer machine
            // the host sampler would hold the daemon at Throttled while
            // the developer types, and reconcile only runs in IdleDrain.
            .env(
                constants::env::VAPOR_THROTTLE_INPUTS,
                constants::engine::THROTTLE_INPUTS_STATIC,
            );
        for (name, value) in &self.extra_env {
            command.env(name, value);
        }
    }
}

/// One scenario's sandbox: a directory holding one or more homes.
#[derive(Clone, Debug)]
pub struct Sandbox {
    pub root: PathBuf,
}

impl Sandbox {
    pub fn create(root: &Path) -> Result<Self, Failure> {
        fs::create_dir_all(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
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
        // the provider (`ensure_cloud_sync_directory`) unless a scenario
        // seeds it first, and the local root by the daemon.
        if let Some(parent) = cloud.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Home {
            label: label.to_string(),
            dir,
            local,
            cloud,
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
