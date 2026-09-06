//! Live reload of `vapor.json`.
//!
//! The daemon composes its pipeline from the configuration once, at
//! start. Some settings can change under a running daemon without
//! recomposing anything: resource ceilings, idle boost, the mass-delete
//! guard, ignore toggles and rules, and the timeline length. Those are
//! `constants::config::LIVE_RELOAD_KEYS`, and the multi-profile runtime
//! applies them within one poll interval of the file changing. Roots,
//! provider, sync direction and the profile set reshape the pipeline;
//! a change to one of those is reported in status as
//! `config_restart_required` until the daemon restarts.
//!
//! The poll is one `stat` per second. A file that fails to load is
//! reported once and left alone: the daemon keeps the configuration it
//! last applied rather than dropping to defaults because of a typo.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use vapor_shared::config::{VaporConfig, load_from};
use vapor_shared::constants;

use crate::logging;

/// What changed between the applied configuration and the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigChange {
    /// Live keys whose values differ; the runtime applies them.
    pub live: Vec<&'static str>,
    /// Restart-required keys whose values differ.
    pub restart: Vec<&'static str>,
}

impl ConfigChange {
    pub fn is_empty(&self) -> bool {
        self.live.is_empty() && self.restart.is_empty()
    }
}

/// Compares two configurations key by key. Keys that neither the
/// daemon nor its budget read (`languageCode`, `autoLaunch`) are not
/// reported: nothing in the daemon changes when they do.
pub fn diff(applied: &VaporConfig, fresh: &VaporConfig) -> ConfigChange {
    use constants::config as keys;
    let mut change = ConfigChange::default();
    let mut live = |differs: bool, key: &'static str| {
        if differs {
            change.live.push(key);
        }
    };
    live(
        applied.use_git_ignore != fresh.use_git_ignore,
        keys::KEY_USE_GIT_IGNORE,
    );
    live(
        applied.use_vapor_ignore != fresh.use_vapor_ignore,
        keys::KEY_USE_VAPOR_IGNORE,
    );
    live(
        applied.pre_ignore_rules != fresh.pre_ignore_rules,
        keys::KEY_PRE_IGNORE_RULES,
    );
    live(
        applied.post_ignore_rules != fresh.post_ignore_rules,
        keys::KEY_POST_IGNORE_RULES,
    );
    live(
        applied.timeline_limit != fresh.timeline_limit,
        keys::KEY_TIMELINE_LIMIT,
    );
    live(
        applied.resource_limits != fresh.resource_limits,
        keys::KEY_RESOURCE_LIMITS,
    );
    live(applied.idle_boost != fresh.idle_boost, keys::KEY_IDLE_BOOST);
    live(applied.safeguards != fresh.safeguards, keys::KEY_SAFEGUARDS);
    live(applied.trash != fresh.trash, keys::KEY_TRASH);

    let mut restart = |differs: bool, key: &'static str| {
        if differs {
            change.restart.push(key);
        }
    };
    restart(
        applied.local_sync_directory != fresh.local_sync_directory,
        keys::KEY_LOCAL_SYNC_DIRECTORY,
    );
    restart(
        applied.cloud_sync_directory != fresh.cloud_sync_directory,
        keys::KEY_CLOUD_SYNC_DIRECTORY,
    );
    restart(applied.provider != fresh.provider, keys::KEY_PROVIDER);
    restart(applied.sync_mode != fresh.sync_mode, keys::KEY_SYNC_MODE);
    restart(applied.profiles != fresh.profiles, keys::KEY_PROFILES);
    change
}

/// Watches one configuration file and yields the changes the runtime
/// has not applied yet.
pub struct ConfigReloader {
    path: PathBuf,
    applied: VaporConfig,
    last_seen: Option<(SystemTime, u64)>,
    last_poll: Option<Instant>,
    poll_interval: Duration,
}

impl ConfigReloader {
    pub fn new(path: &Path, applied: VaporConfig) -> Self {
        Self {
            path: path.to_path_buf(),
            last_seen: file_stamp(path),
            applied,
            last_poll: None,
            poll_interval: Duration::from_millis(constants::engine::CONFIG_RELOAD_POLL_MILLIS),
        }
    }

    pub fn applied(&self) -> &VaporConfig {
        &self.applied
    }

    /// Returns the change and the freshly loaded configuration when the
    /// file changed since the last poll and loads cleanly. Rate-limited
    /// to one `stat` per poll interval.
    pub fn poll(&mut self, now: Instant) -> Option<(ConfigChange, VaporConfig)> {
        if self
            .last_poll
            .is_some_and(|last| now.saturating_duration_since(last) < self.poll_interval)
        {
            return None;
        }
        self.last_poll = Some(now);
        let stamp = file_stamp(&self.path);
        if stamp == self.last_seen {
            return None;
        }
        self.last_seen = stamp;
        let loaded = load_from(&self.path);
        if let Some(issue) = loaded.load_issue {
            logging::warning(
                "Configuration file changed but does not load; keeping the applied configuration",
                &[("path", self.path.display().to_string()), ("issue", issue)],
            );
            return None;
        }
        let change = diff(&self.applied, &loaded.config);
        if change.is_empty() {
            return None;
        }
        self.applied = loaded.config.clone();
        Some((change, loaded.config))
    }
}

fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_classifies_live_and_restart_keys_and_ignores_app_only_keys() {
        let applied = VaporConfig::default();
        let mut fresh = applied.clone();
        fresh.resource_limits.cpu_percent = 5;
        fresh.pre_ignore_rules = "build/".to_string();
        fresh.provider = "gdrive".to_string();
        fresh.language_code = "es".to_string();
        fresh.trash.retention_days = 7;
        let change = diff(&applied, &fresh);
        assert_eq!(
            change.live,
            vec![
                constants::config::KEY_PRE_IGNORE_RULES,
                constants::config::KEY_RESOURCE_LIMITS,
                constants::config::KEY_TRASH,
            ]
        );
        assert_eq!(change.restart, vec![constants::config::KEY_PROVIDER]);
        assert!(diff(&applied, &applied).is_empty());
    }

    #[test]
    fn every_documented_key_is_classified_exactly_once() {
        // The constants drive the CLI hint and this module drives the
        // daemon; the two must name the same keys.
        for key in constants::config::ALL_KEYS {
            let live = constants::config::LIVE_RELOAD_KEYS.contains(key);
            let restart = constants::config::RESTART_REQUIRED_KEYS.contains(key);
            let app_only = matches!(
                *key,
                constants::config::KEY_AUTO_LAUNCH | constants::config::KEY_LANGUAGE_CODE
            );
            assert!(
                usize::from(live) + usize::from(restart) + usize::from(app_only) == 1,
                "{key} must be live, restart-required, or app-only"
            );
        }
    }

    #[test]
    fn reloader_reports_a_file_change_once_and_keeps_bad_files_out() {
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        // Every edit gets an explicit, strictly later mtime: the stamp
        // must never depend on what the filesystem assigns to a rewrite,
        // which on some hosts can equal the value already recorded.
        let epoch = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        write_at(&path, "{}\n", epoch);
        let mut reloader = ConfigReloader::new(&path, VaporConfig::default());
        let start = Instant::now();
        assert!(reloader.poll(start).is_none(), "unchanged file");

        // A later mtime and a different length is a change.
        write_at(
            &path,
            "{\"timelineLimit\": 42}\n",
            epoch + Duration::from_secs(10),
        );
        let (change, fresh) = reloader
            .poll(start + Duration::from_secs(2))
            .expect("change detected");
        assert_eq!(change.live, vec![constants::config::KEY_TIMELINE_LIMIT]);
        assert_eq!(fresh.timeline_limit, 42);
        assert!(
            reloader.poll(start + Duration::from_secs(4)).is_none(),
            "a change is reported once"
        );

        // Same length, later mtime: still a change. Inside the poll
        // interval nothing is even stat-ed.
        write_at(
            &path,
            "{\"timelineLimit\": 43}\n",
            epoch + Duration::from_secs(20),
        );
        assert!(reloader.poll(start + Duration::from_secs(4)).is_none());
        assert!(reloader.poll(start + Duration::from_secs(6)).is_some());

        // A file that fails to load leaves the applied config alone.
        write_at(&path, "{ not json", epoch + Duration::from_secs(30));
        assert!(reloader.poll(start + Duration::from_secs(8)).is_none());
        assert_eq!(reloader.applied().timeline_limit, 43);
    }

    fn write_at(path: &Path, contents: &str, modified_at: std::time::SystemTime) {
        std::fs::write(path, contents).expect("write");
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open")
            .set_modified(modified_at)
            .expect("set mtime");
    }
}
