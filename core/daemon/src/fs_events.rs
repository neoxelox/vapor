use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use notify::event::{CreateKind, ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use vapor_shared::constants;

use crate::logging;
use crate::path_filter::{EventPathFilter, EventPathFilterOptions};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsEventKind {
    Created,
    Modified,
    Removed,
    Renamed,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsEventRecord {
    pub path: PathBuf,
    pub kind: FsEventKind,
    pub observed_at: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsEventErrorRecord {
    pub description: String,
}

pub trait FsEventRecording: Send + Sync + 'static {
    fn record_event(&self, event: FsEventRecord);
    fn record_error(&self, error: FsEventErrorRecord);
}

#[derive(Debug)]
pub struct LoggingFsEventRecorder;

impl FsEventRecording for LoggingFsEventRecorder {
    fn record_event(&self, event: FsEventRecord) {
        logging::debug(
            "Recorded filesystem callback metadata",
            &[
                ("event_kind", format!("{:?}", event.kind)),
                ("path", event.path.display().to_string()),
            ],
        );
    }

    fn record_error(&self, error: FsEventErrorRecord) {
        logging::warning(
            "Filesystem callback returned an error",
            &[("error", error.description)],
        );
    }
}

#[derive(Debug)]
pub enum FsEventsWatcherError {
    EmptyWatchRoot,
    WatchRootMissing(PathBuf),
    WatchRootNotDirectory(PathBuf),
    WatchRootCanonicalizeFailed(PathBuf, std::io::Error),
    Notify(notify::Error),
}

impl Display for FsEventsWatcherError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyWatchRoot => write!(f, "watch root must not be empty"),
            Self::WatchRootMissing(path) => {
                write!(f, "watch root does not exist: {}", path.display())
            }
            Self::WatchRootNotDirectory(path) => {
                write!(f, "watch root is not a directory: {}", path.display())
            }
            Self::WatchRootCanonicalizeFailed(path, error) => {
                write!(
                    f,
                    "failed to canonicalize watch root {}: {error}",
                    path.display()
                )
            }
            Self::Notify(error) => write!(f, "notify watcher error: {error}"),
        }
    }
}

impl Error for FsEventsWatcherError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WatchRootCanonicalizeFailed(_, error) => Some(error),
            Self::Notify(error) => Some(error),
            _ => None,
        }
    }
}

impl From<notify::Error> for FsEventsWatcherError {
    fn from(value: notify::Error) -> Self {
        Self::Notify(value)
    }
}

/// Live, swappable path filter shared between the fs-watch callback and
/// the runtime thread.
///
/// The callback reads it (cheap `RwLock` read) and flags a reload when it
/// observes a change to an ignore file; the runtime thread performs the
/// expensive rebuild (a tree walk + glob compilation) between ticks via
/// [`SharedEventPathFilter::rebuild_if_requested`], so ignore-rule edits
/// take effect without a daemon restart and without heavy work in the
/// callback path.
#[derive(Debug)]
pub struct SharedEventPathFilter {
    watch_root: PathBuf,
    options: EventPathFilterOptions,
    filter: RwLock<EventPathFilter>,
    reload_requested: AtomicBool,
}

impl SharedEventPathFilter {
    pub fn new(watch_root: &Path, options: EventPathFilterOptions) -> Self {
        let filter = EventPathFilter::for_watch_root(watch_root, &options);
        Self {
            watch_root: watch_root.to_path_buf(),
            options,
            filter: RwLock::new(filter),
            reload_requested: AtomicBool::new(false),
        }
    }

    pub fn should_ignore(&self, path: &Path) -> bool {
        self.filter
            .read()
            .expect("SharedEventPathFilter lock poisoned")
            .should_ignore(path)
    }

    /// Called from the fs-watch callback when an ignore file changed.
    pub fn request_reload(&self) {
        self.reload_requested.store(true, Ordering::Release);
    }

    /// Marks an observed path: when it names an *enabled* ignore-file
    /// kind, a reload is requested.
    fn note_observed_path(&self, path: &Path) {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let is_gitignore =
            self.options.use_gitignore && file_name == constants::filtering::GIT_IGNORE_FILE_NAME;
        let is_vaporignore = self.options.use_vaporignore
            && file_name == constants::filtering::VAPOR_IGNORE_FILE_NAME;
        if is_gitignore || is_vaporignore {
            self.request_reload();
        }
    }

    /// Rebuilds the compiled filter when a reload was requested. Runs on
    /// the runtime thread (never the callback); returns whether a rebuild
    /// happened.
    pub fn rebuild_if_requested(&self) -> bool {
        if !self.reload_requested.swap(false, Ordering::AcqRel) {
            return false;
        }

        let rebuilt = EventPathFilter::for_watch_root(&self.watch_root, &self.options);
        *self
            .filter
            .write()
            .expect("SharedEventPathFilter lock poisoned") = rebuilt;
        logging::info(
            "Reloaded ignore-rule path filter after an ignore file changed",
            &[("watch_root", self.watch_root.display().to_string())],
        );
        true
    }
}

pub struct FsEventsWatcher {
    _watcher: RecommendedWatcher,
    watch_root: PathBuf,
    path_filter: Arc<SharedEventPathFilter>,
}

impl FsEventsWatcher {
    pub fn start(
        watch_root: impl Into<PathBuf>,
        recorder: Arc<dyn FsEventRecording>,
        filter_options: EventPathFilterOptions,
    ) -> Result<Self, FsEventsWatcherError> {
        let watch_root = normalize_watch_root(watch_root.into())?;
        let callback_watch_root = watch_root.clone();
        let callback_recorder = Arc::clone(&recorder);
        let path_filter = Arc::new(SharedEventPathFilter::new(&watch_root, filter_options));
        let callback_path_filter = Arc::clone(&path_filter);

        let mut watcher = notify::recommended_watcher(move |result| {
            record_callback_result(
                &callback_watch_root,
                callback_path_filter.as_ref(),
                result,
                callback_recorder.as_ref(),
            );
        })?;

        watcher.watch(&watch_root, RecursiveMode::Recursive)?;
        logging::info(
            "Started recursive filesystem watcher",
            &[("watch_root", watch_root.display().to_string())],
        );

        Ok(Self {
            _watcher: watcher,
            watch_root,
            path_filter,
        })
    }

    pub fn watch_root(&self) -> &Path {
        &self.watch_root
    }

    pub fn path_filter(&self) -> &Arc<SharedEventPathFilter> {
        &self.path_filter
    }
}

pub(crate) fn normalize_watch_root(watch_root: PathBuf) -> Result<PathBuf, FsEventsWatcherError> {
    if watch_root.as_os_str().is_empty() {
        return Err(FsEventsWatcherError::EmptyWatchRoot);
    }

    if !watch_root.exists() {
        return Err(FsEventsWatcherError::WatchRootMissing(watch_root));
    }

    if !watch_root.is_dir() {
        return Err(FsEventsWatcherError::WatchRootNotDirectory(watch_root));
    }

    fs::canonicalize(&watch_root)
        .map_err(|error| FsEventsWatcherError::WatchRootCanonicalizeFailed(watch_root, error))
}

fn record_callback_result(
    watch_root: &Path,
    path_filter: &SharedEventPathFilter,
    result: Result<Event, notify::Error>,
    recorder: &dyn FsEventRecording,
) {
    match result {
        Ok(event) => {
            // A paired rename (`RenameMode::Both`) carries `[from, to]`
            // paths; splitting it into a Removed(from) + Created(to) pair
            // keeps both sides independently actionable — a bare "Renamed"
            // record for a path that no longer exists cannot be executed.
            let per_path_kinds = split_event_kinds(&event);
            for (index, path) in event.paths.iter().enumerate() {
                let kind = per_path_kinds(index);
                if let Some(path) = normalize_event_path(watch_root, path) {
                    path_filter.note_observed_path(&path);
                    if path_filter.should_ignore(&path) {
                        continue;
                    }

                    recorder.record_event(FsEventRecord {
                        path,
                        kind,
                        observed_at: SystemTime::now(),
                    });
                }
            }
        }
        Err(error) => {
            recorder.record_error(FsEventErrorRecord {
                description: error.to_string(),
            });
        }
    }
}

/// Returns a per-path kind selector for the event. Everything except a
/// paired rename maps every path to the same kind.
fn split_event_kinds(event: &Event) -> impl Fn(usize) -> FsEventKind {
    let is_paired_rename = matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
    ) && event.paths.len() == 2;
    let uniform_kind = map_event_kind(&event.kind);
    move |index| {
        if is_paired_rename {
            if index == 0 {
                FsEventKind::Removed
            } else {
                FsEventKind::Created
            }
        } else {
            uniform_kind
        }
    }
}

fn normalize_event_path(watch_root: &Path, event_path: &Path) -> Option<PathBuf> {
    let candidate = if event_path.is_absolute() {
        event_path.to_path_buf()
    } else {
        watch_root.join(event_path)
    };

    let candidate = normalize_absolute_path(candidate)?;

    if candidate.starts_with(watch_root) {
        Some(candidate)
    } else {
        None
    }
}

pub fn resolve_event_path_within_watch_root(watch_root: &Path, candidate: &Path) -> bool {
    path_resolves_within_watch_root(watch_root, candidate)
}

fn normalize_absolute_path(path: PathBuf) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }

    // On Windows we keep the drive-letter or UNC `Component::Prefix` so paths
    // like `C:\Users\alex\Vapor` survive normalization. `Component::Prefix`
    // is never produced on Unix, so the runtime branches are mutually
    // exclusive. The push order intentionally preserves prefix-then-root so
    // the reconstructed path stays absolute on every host.
    let mut normalized = PathBuf::new();
    let mut has_root = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str());
            }
            Component::RootDir => {
                normalized.push(component.as_os_str());
                has_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    if !has_root && !cfg!(windows) {
        return None;
    }

    Some(normalized)
}

fn path_resolves_within_watch_root(watch_root: &Path, candidate: &Path) -> bool {
    let Ok(relative_path) = candidate.strip_prefix(watch_root) else {
        return false;
    };

    let mut resolved_path = watch_root.to_path_buf();
    for component in relative_path.components() {
        let Component::Normal(part) = component else {
            return false;
        };

        let Some(next_path) = resolve_path_step(&resolved_path.join(part), watch_root) else {
            return false;
        };
        resolved_path = next_path;

        if !resolved_path.starts_with(watch_root) {
            return false;
        }
    }

    true
}

fn resolve_path_step(path: &Path, watch_root: &Path) -> Option<PathBuf> {
    let mut resolved = path.to_path_buf();
    for _ in 0..32 {
        match fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = fs::read_link(&resolved).ok()?;
                let target = if target.is_absolute() {
                    target
                } else {
                    resolved.parent().unwrap_or(watch_root).join(target)
                };
                resolved = normalize_absolute_path(target)?;
                if !resolved.starts_with(watch_root) {
                    return None;
                }
            }
            Ok(_) => return Some(resolved),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(resolved),
            Err(_) => return None,
        }
    }

    None
}

fn map_event_kind(kind: &EventKind) -> FsEventKind {
    match kind {
        EventKind::Create(CreateKind::Any)
        | EventKind::Create(CreateKind::File)
        | EventKind::Create(CreateKind::Folder)
        | EventKind::Create(CreateKind::Other) => FsEventKind::Created,
        // Unpaired rename halves are directional and can be mapped to an
        // actionable kind directly. The paired `Both` case is split by
        // `split_event_kinds`; `Any`/`Other` keep the ambiguous `Renamed`
        // kind (the source vs destination side is unknown).
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => FsEventKind::Removed,
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => FsEventKind::Created,
        EventKind::Modify(ModifyKind::Name(_)) => FsEventKind::Renamed,
        EventKind::Modify(_) => FsEventKind::Modified,
        EventKind::Remove(_) => FsEventKind::Removed,
        _ => FsEventKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    #[cfg(windows)]
    #[test]
    fn normalize_preserves_drive_letter_prefix_on_windows() {
        let normalized = normalize_absolute_path(PathBuf::from("C:\\Users\\alex\\Vapor\\file.txt"))
            .expect("normalized drive-letter path");
        assert_eq!(
            normalized,
            PathBuf::from("C:\\Users\\alex\\Vapor\\file.txt")
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_resolves_parent_traversal_on_windows() {
        let normalized = normalize_absolute_path(PathBuf::from("C:\\a\\b\\..\\c"))
            .expect("normalized prefix path with parent");
        assert_eq!(normalized, PathBuf::from("C:\\a\\c"));
    }

    #[cfg(unix)]
    #[test]
    fn normalize_keeps_unix_root_when_no_prefix_present() {
        let normalized = normalize_absolute_path(PathBuf::from("/tmp/vapor-root/file.txt"))
            .expect("normalized unix path");
        assert_eq!(normalized, PathBuf::from("/tmp/vapor-root/file.txt"));
    }

    #[test]
    fn callback_normalizes_relative_paths_and_records_metadata() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = Event {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![PathBuf::from("src/main.rs")],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, FsEventKind::Created);
        assert_eq!(events[0].path, watch_root.join("src/main.rs"));
    }

    #[test]
    fn callback_filters_paths_outside_watch_root() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: vec![
                watch_root
                    .parent()
                    .expect("synthetic watch root has a parent")
                    .join("other-root/file.txt"),
            ],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert!(events.is_empty());
    }

    #[test]
    fn callback_rejects_relative_traversal_outside_watch_root() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: vec![PathBuf::from("../escape.txt")],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert!(events.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected_by_resolve_helper() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        let outside_root = temp_dir.path().join("outside");
        fs::create_dir_all(&watch_root).expect("create watch root");
        fs::create_dir_all(&outside_root).expect("create outside root");
        symlink(&outside_root, watch_root.join("escape")).expect("create escape symlink");

        let candidate = watch_root.join("escape/file.txt");
        assert!(!resolve_event_path_within_watch_root(
            &watch_root,
            &candidate
        ));
    }

    #[cfg(unix)]
    #[test]
    fn nested_symlink_escape_is_rejected_by_resolve_helper() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        let outside_root = temp_dir.path().join("outside");
        fs::create_dir_all(&watch_root).expect("create watch root");
        fs::create_dir_all(&outside_root).expect("create outside root");
        symlink(watch_root.join("second-hop"), watch_root.join("first-hop"))
            .expect("create first hop symlink");
        symlink(&outside_root, watch_root.join("second-hop")).expect("create second hop symlink");

        let candidate = watch_root.join("first-hop/file.txt");
        assert!(!resolve_event_path_within_watch_root(
            &watch_root,
            &candidate
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_resolving_inside_watch_root_is_accepted_by_resolve_helper() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        let target_root = watch_root.join("nested/target");
        fs::create_dir_all(&target_root).expect("create nested target root");
        symlink(watch_root.join("nested"), watch_root.join("alias"))
            .expect("create inside symlink");

        let candidate = watch_root.join("alias/target");
        assert!(resolve_event_path_within_watch_root(
            &watch_root,
            &candidate
        ));
    }

    #[cfg(unix)]
    #[test]
    fn callback_records_lexically_inside_paths_without_blocking_on_filesystem_resolution() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        let outside_root = temp_dir.path().join("outside");
        fs::create_dir_all(&watch_root).expect("create watch root");
        fs::create_dir_all(&outside_root).expect("create outside root");
        symlink(&outside_root, watch_root.join("escape")).expect("create escape symlink");

        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();
        let event = Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: vec![PathBuf::from("escape/file.txt")],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert_eq!(
            events.len(),
            1,
            "callback should record the lexical path; runtime-thread resolver is responsible for dropping symlink-escape events",
        );
    }

    #[cfg(unix)]
    #[test]
    fn watch_root_is_canonicalized_before_use() {
        let temp_dir = TempDir::new().expect("temp dir");
        let real_root = temp_dir.path().join("real-root");
        let symlink_root = temp_dir.path().join("watch-root-link");
        fs::create_dir_all(&real_root).expect("create real root");
        symlink(&real_root, &symlink_root).expect("create root symlink");

        let normalized = normalize_watch_root(symlink_root).expect("normalize watch root");

        assert_eq!(
            normalized,
            fs::canonicalize(&real_root).expect("canonical real root")
        );
    }

    #[test]
    fn callback_maps_ambiguous_rename_events_to_renamed() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::Any)),
            paths: vec![watch_root.join("renamed.txt")],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, FsEventKind::Renamed);
    }

    #[test]
    fn callback_splits_paired_rename_into_removed_source_and_created_destination() {
        // A bare "Renamed" record for the source path (which no longer
        // exists) is unactionable; the pair maps to a delete + create so
        // both sides flow through the normal pipeline.
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::Both)),
            paths: vec![watch_root.join("old.txt"), watch_root.join("new.txt")],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].path, watch_root.join("old.txt"));
        assert_eq!(events[0].kind, FsEventKind::Removed);
        assert_eq!(events[1].path, watch_root.join("new.txt"));
        assert_eq!(events[1].kind, FsEventKind::Created);
    }

    #[test]
    fn callback_maps_directional_rename_halves_to_removed_and_created() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let from_event = Event {
            kind: EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::From)),
            paths: vec![watch_root.join("old.txt")],
            attrs: Default::default(),
        };
        let to_event = Event {
            kind: EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::To)),
            paths: vec![watch_root.join("new.txt")],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(from_event), &recorder);
        record_callback_result(&watch_root, &path_filter, Ok(to_event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert_eq!(events[0].kind, FsEventKind::Removed);
        assert_eq!(events[1].kind, FsEventKind::Created);
    }

    #[test]
    fn observing_an_ignore_file_change_requests_a_filter_reload() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        fs::create_dir_all(&watch_root).expect("create watch root");
        let path_filter = SharedEventPathFilter::new(
            &watch_root,
            EventPathFilterOptions {
                pre_user_rules: Vec::new(),
                post_user_rules: Vec::new(),
                ..EventPathFilterOptions::default()
            },
        );
        let recorder = TestRecorder::default();

        // Before the .gitignore lands, generated/ files flow through.
        assert!(!path_filter.should_ignore(&watch_root.join("generated/app.js")));
        assert!(
            !path_filter.rebuild_if_requested(),
            "no reload requested yet"
        );

        // The .gitignore is written and its own fs event arrives.
        fs::write(watch_root.join(".gitignore"), "generated/\n").expect("write .gitignore");
        let event = Event {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![watch_root.join(".gitignore")],
            attrs: Default::default(),
        };
        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        assert!(
            path_filter.rebuild_if_requested(),
            "ignore-file event must request a reload"
        );
        assert!(
            path_filter.should_ignore(&watch_root.join("generated/app.js")),
            "reloaded filter must apply the new rules"
        );
    }

    #[test]
    fn callback_records_notify_errors() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        record_callback_result(
            &watch_root,
            &path_filter,
            Err(notify::Error::generic("watch callback failed")),
            &recorder,
        );

        let errors = recorder.errors.lock().expect("errors mutex poisoned");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].description.contains("watch callback failed"));
    }

    #[test]
    fn callback_skips_paths_matching_default_ignore_rules() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: vec![PathBuf::from("node_modules/pkg/index.js")],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert!(events.is_empty());
    }

    #[test]
    fn callback_burst_regression_stays_under_guardrail() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let start = Instant::now();
        for index in 0..5_000 {
            let event = Event {
                kind: EventKind::Modify(ModifyKind::Any),
                paths: vec![PathBuf::from(format!("src/file-{index}.rs"))],
                attrs: Default::default(),
            };
            record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);
        }
        let elapsed = start.elapsed();

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert_eq!(events.len(), 5_000);
        assert!(
            elapsed < Duration::from_secs(2),
            "callback burst took {:?}, expected < 2s",
            elapsed
        );
    }

    #[test]
    fn callback_deep_path_regression_stays_under_guardrail() {
        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        let mut deepest_directory = watch_root.clone();
        for index in 0..12 {
            deepest_directory = deepest_directory.join(format!("level-{index}"));
        }
        fs::create_dir_all(&deepest_directory).expect("create deep watch tree");

        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();
        let relative_path = PathBuf::from(
            "level-0/level-1/level-2/level-3/level-4/level-5/level-6/level-7/level-8/level-9/level-10/level-11/file.txt",
        );

        let start = Instant::now();
        for _ in 0..2_000 {
            let event = Event {
                kind: EventKind::Modify(ModifyKind::Any),
                paths: vec![relative_path.clone()],
                attrs: Default::default(),
            };
            record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);
        }
        let elapsed = start.elapsed();

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert_eq!(events.len(), 2_000);
        assert!(
            elapsed < Duration::from_secs(2),
            "deep callback burst took {:?}, expected < 2s",
            elapsed
        );
    }

    #[derive(Default)]
    struct TestRecorder {
        events: Mutex<Vec<FsEventRecord>>,
        errors: Mutex<Vec<FsEventErrorRecord>>,
    }

    impl FsEventRecording for TestRecorder {
        fn record_event(&self, event: FsEventRecord) {
            self.events
                .lock()
                .expect("events mutex poisoned")
                .push(event);
        }

        fn record_error(&self, error: FsEventErrorRecord) {
            self.errors
                .lock()
                .expect("errors mutex poisoned")
                .push(error);
        }
    }

    fn test_path_filter(watch_root: &Path) -> SharedEventPathFilter {
        let options = EventPathFilterOptions {
            use_gitignore: false,
            use_vaporignore: false,
            ..EventPathFilterOptions::default()
        };
        SharedEventPathFilter::new(watch_root, options)
    }

    /// A host-absolute synthetic watch root for the callback tests, which
    /// inject events without touching the real filesystem. The root must be
    /// absolute on the current OS: a Unix-style `/tmp/...` literal is not an
    /// absolute path on Windows, so the watch-root prefix check would drop
    /// every event and the tests would observe zero recordings.
    fn synthetic_watch_root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("C:\\vapor-root")
        } else {
            PathBuf::from("/tmp/vapor-root")
        }
    }
}
