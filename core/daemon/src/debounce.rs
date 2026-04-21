use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use vapor_shared::constants;

use crate::event_intents::{
    BoundedEventIntentMaps, BoundedFsEventRecorder, PendingEventFlags, PendingEventRecord,
};
use crate::fs_events::FsEventKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebounceClass {
    KeyConfig,
    CodeText,
    Lockfile,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebounceWindows {
    minimum: Duration,
    maximum: Duration,
    key_config: Duration,
    code_text: Duration,
    lockfile: Duration,
    other: Duration,
}

impl Default for DebounceWindows {
    fn default() -> Self {
        Self::from_millis(
            constants::engine::MIN_DEBOUNCE_WINDOW_MILLIS,
            constants::engine::MAX_DEBOUNCE_WINDOW_MILLIS,
            constants::engine::KEY_CONFIG_DEBOUNCE_WINDOW_MILLIS,
            constants::engine::CODE_TEXT_DEBOUNCE_WINDOW_MILLIS,
            constants::engine::LOCKFILE_DEBOUNCE_WINDOW_MILLIS,
            constants::engine::DEFAULT_DEBOUNCE_WINDOW_MILLIS,
        )
    }
}

impl DebounceWindows {
    pub fn from_millis(
        minimum_millis: u64,
        maximum_millis: u64,
        key_config_millis: u64,
        code_text_millis: u64,
        lockfile_millis: u64,
        other_millis: u64,
    ) -> Self {
        let minimum = Duration::from_millis(minimum_millis);
        let maximum = Duration::from_millis(maximum_millis);
        let (minimum, maximum) = if minimum <= maximum {
            (minimum, maximum)
        } else {
            (maximum, minimum)
        };

        Self {
            minimum,
            maximum,
            key_config: clamp_duration(Duration::from_millis(key_config_millis), minimum, maximum),
            code_text: clamp_duration(Duration::from_millis(code_text_millis), minimum, maximum),
            lockfile: clamp_duration(Duration::from_millis(lockfile_millis), minimum, maximum),
            other: clamp_duration(Duration::from_millis(other_millis), minimum, maximum),
        }
    }

    pub fn classify_path(&self, path: &Path) -> (DebounceClass, Duration) {
        if is_lockfile_path(path) {
            return (DebounceClass::Lockfile, self.lockfile);
        }

        if is_key_config_path(path) {
            return (DebounceClass::KeyConfig, self.key_config);
        }

        if is_code_or_text_path(path) {
            return (DebounceClass::CodeText, self.code_text);
        }

        (DebounceClass::Other, self.other)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StabilizedEvent {
    pub path: PathBuf,
    pub first_observed_at: SystemTime,
    pub last_observed_at: SystemTime,
    pub last_event_kind: FsEventKind,
    pub flags: PendingEventFlags,
    pub burst_count: usize,
    pub debounce_class: DebounceClass,
    pub quiet_window: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebounceLoop {
    tick_interval: Duration,
    windows: DebounceWindows,
    last_tick_at: Option<SystemTime>,
}

impl Default for DebounceLoop {
    fn default() -> Self {
        Self::new()
    }
}

impl DebounceLoop {
    pub fn new() -> Self {
        Self::with_windows(DebounceWindows::default())
    }

    pub fn with_windows(windows: DebounceWindows) -> Self {
        Self {
            tick_interval: Duration::from_millis(constants::engine::DEBOUNCE_TICK_MILLIS),
            windows,
            last_tick_at: None,
        }
    }

    pub fn tick_interval(&self) -> Duration {
        self.tick_interval
    }

    pub fn last_tick_at(&self) -> Option<SystemTime> {
        self.last_tick_at
    }

    pub fn windows(&self) -> &DebounceWindows {
        &self.windows
    }

    pub fn run_tick(
        &mut self,
        maps: &mut BoundedEventIntentMaps,
        now: SystemTime,
    ) -> Vec<StabilizedEvent> {
        if !self.tick_is_due(now) {
            return Vec::new();
        }

        self.last_tick_at = Some(now);
        maps.drain_ready_events_with(|record| self.classify_stable_record(record, now))
            .into_iter()
            .map(|(record, (debounce_class, quiet_window))| {
                self.build_stabilized_event(record, debounce_class, quiet_window)
            })
            .collect()
    }

    pub fn run_tick_for_recorder(
        &mut self,
        recorder: &BoundedFsEventRecorder,
        now: SystemTime,
    ) -> Vec<StabilizedEvent> {
        if !self.tick_is_due(now) {
            return Vec::new();
        }

        self.last_tick_at = Some(now);
        let ready_records = recorder.with_mut_state(|maps| {
            maps.drain_ready_events_with(|record| self.classify_stable_record(record, now))
        });

        ready_records
            .into_iter()
            .map(|(record, (debounce_class, quiet_window))| {
                self.build_stabilized_event(record, debounce_class, quiet_window)
            })
            .collect()
    }

    fn tick_is_due(&self, now: SystemTime) -> bool {
        let Some(last_tick_at) = self.last_tick_at else {
            return true;
        };

        match now.duration_since(last_tick_at) {
            Ok(elapsed) => elapsed >= self.tick_interval,
            Err(_) => true,
        }
    }

    fn classify_stable_record(
        &self,
        record: &PendingEventRecord,
        now: SystemTime,
    ) -> Option<(DebounceClass, Duration)> {
        let classification @ (_, quiet_window) = self.windows.classify_path(record.path.as_path());
        match now.duration_since(record.last_observed_at) {
            Ok(elapsed) if elapsed >= quiet_window => Some(classification),
            Ok(_) | Err(_) => None,
        }
    }

    fn build_stabilized_event(
        &self,
        record: PendingEventRecord,
        debounce_class: DebounceClass,
        quiet_window: Duration,
    ) -> StabilizedEvent {
        StabilizedEvent {
            path: record.path,
            first_observed_at: record.first_observed_at,
            last_observed_at: record.last_observed_at,
            last_event_kind: record.last_event_kind,
            flags: record.flags,
            burst_count: record.burst_count,
            debounce_class,
            quiet_window,
        }
    }
}

fn clamp_duration(duration: Duration, minimum: Duration, maximum: Duration) -> Duration {
    if duration < minimum {
        minimum
    } else if duration > maximum {
        maximum
    } else {
        duration
    }
}

fn is_lockfile_path(path: &Path) -> bool {
    file_name_matches(
        path,
        &[
            "bun.lock",
            "bun.lockb",
            "cargo.lock",
            "composer.lock",
            "gemfile.lock",
            "package-lock.json",
            "package.resolved",
            "pipfile.lock",
            "pnpm-lock.yaml",
            "podfile.lock",
            "poetry.lock",
            "uv.lock",
            "yarn.lock",
        ],
    ) || extension_matches(path, &["lock"])
}

fn is_key_config_path(path: &Path) -> bool {
    let Some(file_name) = file_name_str(path) else {
        return false;
    };

    if starts_with_ignore_ascii_case(file_name, ".env") {
        return true;
    }

    matches_ignore_ascii_case(
        file_name,
        &[
            ".gitignore",
            ".swiftformat",
            ".swiftlint.yml",
            constants::filtering::VAPOR_IGNORE_FILE_NAME,
            "cargo.toml",
            "dockerfile",
            "info.plist",
            "justfile",
            "makefile",
            "package.json",
            "tsconfig.json",
            constants::runtime::CONFIGURATION_FILE_NAME,
        ],
    ) || extension_matches(
        path,
        &["cfg", "conf", "ini", "json", "plist", "toml", "yaml", "yml"],
    )
}

fn is_code_or_text_path(path: &Path) -> bool {
    extension_matches(
        path,
        &[
            "bash", "c", "cc", "cpp", "css", "csv", "cxx", "go", "h", "hh", "hpp", "html", "hxx",
            "java", "js", "jsx", "kt", "kts", "less", "m", "md", "mm", "php", "proto", "py", "rb",
            "rs", "sass", "scala", "scss", "sh", "sql", "swift", "svelte", "svg", "ts", "tsx",
            "txt", "xml", "zsh",
        ],
    )
}

fn file_name_str(path: &Path) -> Option<&str> {
    path.file_name().and_then(|value| value.to_str())
}

fn extension_str(path: &Path) -> Option<&str> {
    path.extension().and_then(|value| value.to_str())
}

fn file_name_matches(path: &Path, candidates: &[&str]) -> bool {
    file_name_str(path)
        .map(|value| matches_ignore_ascii_case(value, candidates))
        .unwrap_or(false)
}

fn extension_matches(path: &Path, candidates: &[&str]) -> bool {
    extension_str(path)
        .map(|value| matches_ignore_ascii_case(value, candidates))
        .unwrap_or(false)
}

fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .map(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_intents::{EventIntentLimits, PendingIntentKind};
    use crate::fs_events::{FsEventErrorRecord, FsEventKind, FsEventRecord, FsEventRecording};
    use std::time::{Instant, UNIX_EPOCH};

    #[test]
    fn default_loop_uses_250ms_tick_interval() {
        let loop_state = DebounceLoop::default();
        assert_eq!(loop_state.tick_interval(), Duration::from_millis(250));
    }

    #[test]
    fn debounce_windows_clamp_to_safe_bounds() {
        let windows = DebounceWindows::from_millis(8_000, 500, 100, 900, 12_000, 50);

        assert_eq!(
            windows.classify_path(Path::new("/tmp/config.json")),
            (DebounceClass::KeyConfig, Duration::from_millis(500))
        );
        assert_eq!(
            windows.classify_path(Path::new("/tmp/file.swift")),
            (DebounceClass::CodeText, Duration::from_millis(900))
        );
        assert_eq!(
            windows.classify_path(Path::new("/tmp/Cargo.lock")),
            (DebounceClass::Lockfile, Duration::from_millis(8_000))
        );
        assert_eq!(
            windows.classify_path(Path::new("/tmp/video.mov")),
            (DebounceClass::Other, Duration::from_millis(500))
        );
    }

    #[test]
    fn classify_path_prefers_lockfiles_then_configs_then_code() {
        let windows = DebounceWindows::default();

        assert_eq!(
            windows.classify_path(Path::new("/tmp/Cargo.lock")).0,
            DebounceClass::Lockfile
        );
        assert_eq!(
            windows.classify_path(Path::new("/tmp/package.json")).0,
            DebounceClass::KeyConfig
        );
        assert_eq!(
            windows.classify_path(Path::new("/tmp/lib.rs")).0,
            DebounceClass::CodeText
        );
        assert_eq!(
            windows.classify_path(Path::new("/tmp/archive.mov")).0,
            DebounceClass::Other
        );
    }

    #[test]
    fn tick_waits_until_interval_before_emitting_ready_events() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path = watch_root.join("src/lib.rs");
        let mut maps = bounded_maps(&watch_root);
        let mut loop_state = DebounceLoop::default();

        maps.record_event(fs_event(path.clone(), FsEventKind::Modified, 0));

        assert!(loop_state.run_tick(&mut maps, timestamp(1_000)).is_empty());
        assert!(loop_state.run_tick(&mut maps, timestamp(1_200)).is_empty());

        let stabilized = loop_state.run_tick(&mut maps, timestamp(1_250));
        assert_eq!(stabilized.len(), 1);
        assert_eq!(stabilized[0].path, path);
        assert_eq!(stabilized[0].debounce_class, DebounceClass::CodeText);
        assert_eq!(stabilized[0].last_event_kind, FsEventKind::Modified);
        assert_eq!(stabilized[0].quiet_window, Duration::from_millis(1_200));
    }

    #[test]
    fn key_configs_stabilize_before_code_text_paths() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let config_path = watch_root.join("package.json");
        let code_path = watch_root.join("src/main.rs");
        let mut maps = bounded_maps(&watch_root);
        let mut loop_state = DebounceLoop::default();

        maps.record_event(fs_event(config_path.clone(), FsEventKind::Modified, 0));
        maps.record_event(fs_event(code_path.clone(), FsEventKind::Modified, 0));

        let stabilized = loop_state.run_tick(&mut maps, timestamp(1_000));
        assert_eq!(stabilized.len(), 1);
        assert_eq!(stabilized[0].path, config_path);
        assert_eq!(stabilized[0].debounce_class, DebounceClass::KeyConfig);
        assert!(maps.pending_event(&code_path).is_some());
    }

    #[test]
    fn other_paths_wait_for_longer_default_window() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path = watch_root.join("capture.mov");
        let mut maps = bounded_maps(&watch_root);
        let mut loop_state = DebounceLoop::default();

        maps.record_event(fs_event(path.clone(), FsEventKind::Created, 0));

        assert!(loop_state.run_tick(&mut maps, timestamp(3_750)).is_empty());
        let stabilized = loop_state.run_tick(&mut maps, timestamp(4_000));
        assert_eq!(stabilized.len(), 1);
        assert_eq!(stabilized[0].path, path);
        assert_eq!(stabilized[0].debounce_class, DebounceClass::Other);
        assert_eq!(stabilized[0].quiet_window, Duration::from_millis(4_000));
    }

    #[test]
    fn stabilized_events_are_removed_but_existing_intents_remain_tracked() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path = watch_root.join("src/main.rs");
        let mut maps = bounded_maps(&watch_root);
        let mut loop_state = DebounceLoop::default();

        maps.record_event(fs_event(path.clone(), FsEventKind::Modified, 0));
        maps.upsert_intent(path.clone(), PendingIntentKind::Upload, timestamp(50));

        let stabilized = loop_state.run_tick(&mut maps, timestamp(1_250));
        assert_eq!(stabilized.len(), 1);
        assert!(maps.pending_event(&path).is_none());
        assert!(maps.pending_intent(&path).is_some());
        assert_eq!(maps.tracked_path_count(), 1);
    }

    #[test]
    fn run_tick_for_recorder_uses_shared_pending_state() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let recorder =
            BoundedFsEventRecorder::with_limits(watch_root.clone(), EventIntentLimits::new(20, 20));
        let mut loop_state = DebounceLoop::default();

        recorder.record_event(fs_event(
            watch_root.join("src/main.rs"),
            FsEventKind::Modified,
            0,
        ));
        recorder.record_error(FsEventErrorRecord {
            description: "callback failed".to_string(),
        });

        let stabilized = loop_state.run_tick_for_recorder(&recorder, timestamp(1_250));
        assert_eq!(stabilized.len(), 1);
        assert_eq!(recorder.error_count(), 1);
        recorder.with_state(|maps| {
            assert_eq!(maps.pending_event_count(), 0);
        });
    }

    #[test]
    fn tick_is_due_fires_after_system_clock_rewind_instead_of_stalling() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path = watch_root.join("src/main.rs");
        let mut maps = bounded_maps(&watch_root);
        let mut loop_state = DebounceLoop::default();

        maps.record_event(fs_event(path.clone(), FsEventKind::Modified, 0));
        loop_state.run_tick(&mut maps, timestamp(10_000));

        let rewound = loop_state.run_tick(&mut maps, timestamp(1_000));
        let _ = rewound;
        assert_eq!(
            loop_state.last_tick_at(),
            Some(timestamp(1_000)),
            "clock rewind must update the last tick timestamp so later ticks still progress",
        );
    }

    #[test]
    fn debounce_tick_regression_stays_under_guardrail() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let mut maps = BoundedEventIntentMaps::with_limits_and_storm_thresholds(
            watch_root.clone(),
            EventIntentLimits::new(10_000, 10_000),
            crate::storm::StormThresholds {
                directory_unique_paths_threshold: usize::MAX,
                directory_event_count_threshold: usize::MAX,
                global_pending_event_count_threshold: usize::MAX,
                ..crate::storm::StormThresholds::default()
            },
        );
        let mut loop_state = DebounceLoop::default();

        for index in 0..5_000 {
            maps.record_event(fs_event(
                watch_root.join(format!("src/file-{index}.rs")),
                FsEventKind::Modified,
                0,
            ));
        }

        let start = Instant::now();
        let stabilized = loop_state.run_tick(&mut maps, timestamp(1_250));
        let elapsed = start.elapsed();

        assert_eq!(stabilized.len(), 5_000);
        assert!(
            elapsed < Duration::from_secs(2),
            "debounce tick took {:?}, expected < 2s",
            elapsed
        );
    }

    fn bounded_maps(watch_root: &Path) -> BoundedEventIntentMaps {
        BoundedEventIntentMaps::with_limits(
            watch_root.to_path_buf(),
            EventIntentLimits::new(20, 20),
        )
    }

    fn fs_event(path: PathBuf, kind: FsEventKind, milliseconds: u64) -> FsEventRecord {
        FsEventRecord {
            path,
            kind,
            observed_at: timestamp(milliseconds),
        }
    }

    fn timestamp(milliseconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(milliseconds)
    }
}
