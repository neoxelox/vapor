//! Live reload of `vapor.json`.
//!
//! The daemon composes its pipeline from the configuration once, at
//! start. Some settings can change under a running daemon without
//! recomposing anything: resource ceilings, idle boost, ignore toggles
//! and rules, and the timeline length. Those are
//! `constants::config::LIVE_RELOAD_KEYS`, and the multi-profile runtime
//! applies them within one poll interval of the file changing. So is
//! the profile set: a profile that joins is composed and one that
//! leaves ends. The roots, the provider, and the sync direction of a
//! profile that runs reshape its pipeline; a change to one of those is
//! reported in status as `restartRequired` until the daemon restarts.
//!
//! The poll is one `stat` per second. A file that does not parse is
//! reported and left alone: the daemon keeps the configuration it last
//! applied rather than dropping to defaults because of a typo. A file
//! that parses with some bad keys still applies its valid keys; a key
//! whose new value is invalid keeps its applied value, and an unknown
//! key is reported and ignored.

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
/// daemon nor its budget read (`language`, `autoLaunch`) are not
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
        applied.use_gitignore != fresh.use_gitignore,
        keys::KEY_USE_GITIGNORE,
    );
    live(
        applied.use_vaporignore != fresh.use_vaporignore,
        keys::KEY_USE_VAPORIGNORE,
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
    live(applied.trash != fresh.trash, keys::KEY_TRASH);

    let mut restart = |differs: bool, key: &'static str| {
        if differs {
            change.restart.push(key);
        }
    };
    restart(applied.local_root != fresh.local_root, keys::KEY_LOCAL_ROOT);
    restart(applied.cloud_root != fresh.cloud_root, keys::KEY_CLOUD_ROOT);
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
    /// The `deviceId` the daemon started with; it names conflict copies
    /// and op-ids, so a new one waits for a restart.
    started_device_id: Option<String>,
    last_seen: Option<(SystemTime, u64)>,
    /// The last poll found the file gone or empty and did not act on
    /// it yet.
    saw_gone: bool,
    last_poll: Option<Instant>,
    poll_interval: Duration,
}

impl ConfigReloader {
    pub fn new(path: &Path, applied: VaporConfig) -> Self {
        Self {
            path: path.to_path_buf(),
            last_seen: file_stamp(path),
            started_device_id: vapor_shared::device_id::persisted(path),
            applied,
            saw_gone: false,
            last_poll: None,
            poll_interval: Duration::from_millis(constants::engine::CONFIG_RELOAD_POLL_MILLIS),
        }
    }

    pub fn applied(&self) -> &VaporConfig {
        &self.applied
    }

    /// Returns the change and the freshly loaded configuration when the
    /// file changed since the last poll and parses. Rate-limited to one
    /// `stat` per poll interval.
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
            self.saw_gone = false;
            return None;
        }
        // A file that is gone or empty names no profile, and the daemon
        // then ends every profile that runs. A writer that truncates
        // the file and then writes it shows the same for a moment, so
        // this state counts only when the next poll finds it again.
        let gone = stamp.is_none_or(|(_, length)| length == 0);
        if gone && !self.saw_gone {
            self.saw_gone = true;
            return None;
        }
        self.saw_gone = false;
        self.last_seen = stamp;
        let loaded = load_from(&self.path);
        if loaded.unreadable {
            logging::warning(
                "Configuration file changed but does not parse; keeping the applied configuration",
                &[
                    ("path", self.path.display().to_string()),
                    ("issue", loaded.load_issue.unwrap_or_default()),
                ],
            );
            return None;
        }
        if let Some(issue) = &loaded.load_issue {
            logging::warning(
                "Configuration file has keys Vapor cannot use; applying the rest and keeping the applied value of each invalid key",
                &[
                    ("path", self.path.display().to_string()),
                    ("issue", issue.clone()),
                ],
            );
        }
        let mut fresh = loaded.config;
        for key in &loaded.invalid_keys {
            fresh.take_key_from(&self.applied, key);
        }
        let mut change = diff(&self.applied, &fresh);
        if vapor_shared::device_id::persisted(&self.path) != self.started_device_id {
            change.restart.push(constants::config::KEY_DEVICE_ID);
        }
        if change.is_empty() {
            return None;
        }
        self.applied = fresh.clone();
        Some((change, fresh))
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
        fresh.pre_ignore_rules = vec!["build/".to_string()];
        fresh.provider = "gdrive".to_string();
        fresh.language = "es".to_string();
        fresh.trash.retention_days = 14;
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
                constants::config::KEY_AUTO_LAUNCH | constants::config::KEY_LANGUAGE
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

    #[test]
    fn a_file_that_is_empty_for_one_poll_changes_nothing() {
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        let epoch = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let with_roots = "{\"localRoot\": \"/a\", \"cloudRoot\": \"/b\"}\n";
        write_at(&path, with_roots, epoch);
        let mut reloader = ConfigReloader::new(&path, load_from(&path).config);
        let start = Instant::now();

        // A writer truncated the file and has not written it yet.
        write_at(&path, "", epoch + Duration::from_secs(10));
        assert!(
            reloader.poll(start + Duration::from_secs(2)).is_none(),
            "an empty file seen one time is a file that is being written"
        );
        write_at(&path, with_roots, epoch + Duration::from_secs(11));
        assert!(
            reloader.poll(start + Duration::from_secs(4)).is_none(),
            "the file is back as it was: nothing changed"
        );

        // A file that stays empty, or goes, is what the user did.
        write_at(&path, "", epoch + Duration::from_secs(20));
        assert!(reloader.poll(start + Duration::from_secs(6)).is_none());
        let (change, _) = reloader
            .poll(start + Duration::from_secs(8))
            .expect("the second look acts on it");
        assert_eq!(
            change.restart,
            vec![
                constants::config::KEY_LOCAL_ROOT,
                constants::config::KEY_CLOUD_ROOT
            ]
        );
    }

    #[test]
    fn a_bad_key_never_blocks_the_valid_ones() {
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        let epoch = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        write_at(&path, "{\"postIgnoreRules\": [\"private/\"]}\n", epoch);
        let applied = load_from(&path).config;
        let mut reloader = ConfigReloader::new(&path, applied);
        let start = Instant::now();

        // A leftover old key, a rule list in the old string shape, and
        // one good change: the good change applies, and the rules keep
        // their applied value instead of dropping to the default.
        write_at(
            &path,
            "{\"languageCode\": \"en\", \"postIgnoreRules\": \"private/\", \"timelineLimit\": 42}\n",
            epoch + Duration::from_secs(10),
        );
        let (change, fresh) = reloader.poll(start).expect("the valid key applies");
        assert_eq!(change.live, vec![constants::config::KEY_TIMELINE_LIMIT]);
        assert_eq!(fresh.timeline_limit, 42);
        assert_eq!(fresh.post_ignore_rules, vec!["private/"]);
    }

    #[test]
    fn a_new_device_id_waits_for_a_restart() {
        let temp = tempfile::TempDir::new().expect("temp");
        let path = temp.path().join("vapor.json");
        let epoch = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        write_at(&path, "{\"deviceId\": \"mac-studio\"}\n", epoch);
        let mut reloader = ConfigReloader::new(&path, VaporConfig::default());

        write_at(
            &path,
            "{\"deviceId\": \"studio-2\"}\n",
            epoch + Duration::from_secs(10),
        );
        let (change, _) = reloader.poll(Instant::now()).expect("a change");
        assert_eq!(change.restart, vec![constants::config::KEY_DEVICE_ID]);
        assert!(change.live.is_empty());
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
