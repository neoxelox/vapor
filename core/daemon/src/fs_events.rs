use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use vapor_platform::fs_watch::{
    FsWatcher, FsWatcherError, WatchErrorHandler, WatchEvent, WatchEventKind,
    start_native_watcher_with_error_handler,
};
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
    /// The shared filter was built for a different root than the watcher's;
    /// its `strip_prefix` would fail for every event and silently disable
    /// all ignore rules. A hard error so misuse fails loudly on every
    /// build profile, not only under `debug_assert`.
    FilterRootMismatch {
        watch_root: PathBuf,
        filter_root: PathBuf,
    },
    Platform(FsWatcherError),
    /// The bridge thread that drains platform watch events could not be
    /// spawned.
    BridgeSpawn(std::io::Error),
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
            Self::FilterRootMismatch {
                watch_root,
                filter_root,
            } => write!(
                f,
                "path filter root {} does not match watch root {}",
                filter_root.display(),
                watch_root.display()
            ),
            Self::Platform(error) => write!(f, "platform fs-watch error: {error}"),
            Self::BridgeSpawn(error) => {
                write!(f, "cannot spawn fs-watch bridge thread: {error}")
            }
        }
    }
}

impl Error for FsEventsWatcherError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WatchRootCanonicalizeFailed(_, error) => Some(error),
            Self::Platform(error) => Some(error),
            Self::BridgeSpawn(error) => Some(error),
            _ => None,
        }
    }
}

impl From<FsWatcherError> for FsEventsWatcherError {
    fn from(value: FsWatcherError) -> Self {
        Self::Platform(value)
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

    /// Canonical root the filter's relative-path matching is anchored
    /// to. The multi-profile runtime keys shared filter instances by
    /// this root so profiles watching the same directory reload
    /// together.
    pub fn watch_root(&self) -> &Path {
        &self.watch_root
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

/// The daemon's local watch: the platform [`FsWatcher`] owns the OS
/// notifier (one OS-event→kind mapping for every surface, contract-
/// tested in `core/platform`), and a bridge thread drains its channel
/// through the daemon-side discipline — path normalization, ignore
/// filtering, ignore-file reload notes, recorder push. The OS callback
/// itself only normalizes the kind and sends; everything heavier runs
/// on the bridge.
pub struct FsEventsWatcher {
    /// Dropped before the bridge joins so the event channel disconnects
    /// and the bridge exits.
    watcher: Option<Box<dyn FsWatcher>>,
    bridge: Option<std::thread::JoinHandle<()>>,
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
        let path_filter = Arc::new(SharedEventPathFilter::new(&watch_root, filter_options));
        Self::start_with_shared_filter(watch_root, recorder, path_filter)
    }

    /// Starts the watcher around a caller-owned filter so the runtime's
    /// non-callback consumers (reconcile walk, remote-change mapping)
    /// observe the exact same rules — including ignore-file reloads —
    /// as the callback path. The filter must be anchored to the same
    /// canonical root.
    pub fn start_with_shared_filter(
        watch_root: impl Into<PathBuf>,
        recorder: Arc<dyn FsEventRecording>,
        path_filter: Arc<SharedEventPathFilter>,
    ) -> Result<Self, FsEventsWatcherError> {
        let watch_root = normalize_watch_root(watch_root.into())?;
        if path_filter.watch_root() != watch_root.as_path() {
            return Err(FsEventsWatcherError::FilterRootMismatch {
                watch_root: watch_root.clone(),
                filter_root: path_filter.watch_root().to_path_buf(),
            });
        }
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let error_recorder = Arc::clone(&recorder);
        let on_error: WatchErrorHandler = Arc::new(move |description: &str| {
            error_recorder.record_error(FsEventErrorRecord {
                description: description.to_string(),
            });
        });
        let watcher = start_native_watcher_with_error_handler(
            watch_root.clone(),
            event_sender,
            Some(on_error),
        )?;

        let bridge_watch_root = watch_root.clone();
        let bridge_recorder = Arc::clone(&recorder);
        let bridge_path_filter = Arc::clone(&path_filter);
        let bridge = std::thread::Builder::new()
            .name("vapor-fswatch-bridge".to_string())
            .spawn(move || {
                bridge_watch_events(
                    event_receiver,
                    &bridge_watch_root,
                    bridge_path_filter.as_ref(),
                    bridge_recorder.as_ref(),
                );
            })
            .map_err(FsEventsWatcherError::BridgeSpawn)?;

        logging::info(
            "Started recursive filesystem watcher",
            &[("watch_root", watch_root.display().to_string())],
        );

        Ok(Self {
            watcher: Some(watcher),
            bridge: Some(bridge),
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

impl Drop for FsEventsWatcher {
    fn drop(&mut self) {
        // Drop the platform watcher first: its callback sender closes,
        // the bridge's recv() disconnects, and the thread exits.
        self.watcher = None;
        if let Some(bridge) = self.bridge.take() {
            let _ = bridge.join();
        }
    }
}

/// Drains platform watch events until the watcher (the only sender) is
/// dropped.
fn bridge_watch_events(
    events: Receiver<WatchEvent>,
    watch_root: &Path,
    path_filter: &SharedEventPathFilter,
    recorder: &dyn FsEventRecording,
) {
    while let Ok(event) = events.recv() {
        record_watch_event(watch_root, path_filter, event, recorder);
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

fn record_watch_event(
    watch_root: &Path,
    path_filter: &SharedEventPathFilter,
    event: WatchEvent,
    recorder: &dyn FsEventRecording,
) {
    let kind = fs_event_kind(event.kind);
    if let Some(path) = normalize_event_path(watch_root, &event.path) {
        if path_filter.should_ignore(&path) {
            return;
        }
        // Note the observed path (ignore-file reload trigger)
        // only for non-ignored paths: an ignore file inside an
        // excluded directory is never read during a rebuild, so
        // requesting one would be pure waste (and a package
        // install writing many such files would rebuild the
        // whole filter on nearly every tick).
        path_filter.note_observed_path(&path);

        recorder.record_event(FsEventRecord {
            path,
            kind,
            observed_at: event.observed_at,
        });
    }
}

/// 1:1 vocabulary mapping. Rename directionality is already resolved by
/// the platform layer (rename-away → `Removed`, rename-in → `Created`,
/// paired renames split); only the ambiguous `Renamed` survives here and
/// resolves via reconcile.
fn fs_event_kind(kind: WatchEventKind) -> FsEventKind {
    match kind {
        WatchEventKind::Created => FsEventKind::Created,
        WatchEventKind::Modified => FsEventKind::Modified,
        WatchEventKind::Removed => FsEventKind::Removed,
        WatchEventKind::Renamed => FsEventKind::Renamed,
        WatchEventKind::Other => FsEventKind::Other,
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

        let event = WatchEvent {
            path: PathBuf::from("src/main.rs"),
            kind: WatchEventKind::Created,
            observed_at: SystemTime::now(),
        };

        record_watch_event(&watch_root, &path_filter, event, &recorder);

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

        let event = WatchEvent {
            path: watch_root
                .parent()
                .expect("synthetic watch root has a parent")
                .join("other-root/file.txt"),
            kind: WatchEventKind::Modified,
            observed_at: SystemTime::now(),
        };

        record_watch_event(&watch_root, &path_filter, event, &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert!(events.is_empty());
    }

    #[test]
    fn callback_rejects_relative_traversal_outside_watch_root() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = WatchEvent {
            path: PathBuf::from("../escape.txt"),
            kind: WatchEventKind::Modified,
            observed_at: SystemTime::now(),
        };

        record_watch_event(&watch_root, &path_filter, event, &recorder);

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
        let event = WatchEvent {
            path: PathBuf::from("escape/file.txt"),
            kind: WatchEventKind::Modified,
            observed_at: SystemTime::now(),
        };

        record_watch_event(&watch_root, &path_filter, event, &recorder);

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

        let event = WatchEvent {
            path: watch_root.join("renamed.txt"),
            kind: WatchEventKind::Renamed,
            observed_at: SystemTime::now(),
        };

        record_watch_event(&watch_root, &path_filter, event, &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, FsEventKind::Renamed);
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
        let event = WatchEvent {
            path: watch_root.join(".gitignore"),
            kind: WatchEventKind::Created,
            observed_at: SystemTime::now(),
        };
        record_watch_event(&watch_root, &path_filter, event, &recorder);

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
    fn callback_skips_paths_matching_default_ignore_rules() {
        let watch_root = synthetic_watch_root();
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = WatchEvent {
            path: PathBuf::from("node_modules/pkg/index.js"),
            kind: WatchEventKind::Modified,
            observed_at: SystemTime::now(),
        };

        record_watch_event(&watch_root, &path_filter, event, &recorder);

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
            let event = WatchEvent {
                path: PathBuf::from(format!("src/file-{index}.rs")),
                kind: WatchEventKind::Modified,
                observed_at: SystemTime::now(),
            };
            record_watch_event(&watch_root, &path_filter, event, &recorder);
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
            let event = WatchEvent {
                path: relative_path.clone(),
                kind: WatchEventKind::Modified,
                observed_at: SystemTime::now(),
            };
            record_watch_event(&watch_root, &path_filter, event, &recorder);
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

    #[test]
    fn watcher_bridge_delivers_real_fs_events_to_the_recorder() {
        struct ChannelRecorder(std::sync::mpsc::Sender<FsEventRecord>);
        impl FsEventRecording for ChannelRecorder {
            fn record_event(&self, event: FsEventRecord) {
                let _ = self.0.send(event);
            }
            fn record_error(&self, _error: FsEventErrorRecord) {}
        }

        let temp_dir = TempDir::new().expect("temp dir");
        let watch_root = temp_dir.path().join("watch");
        fs::create_dir_all(&watch_root).expect("create watch root");
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = FsEventsWatcher::start(
            watch_root,
            Arc::new(ChannelRecorder(tx)),
            EventPathFilterOptions::default(),
        )
        .expect("start watcher");

        let file = watcher.watch_root().join("hello.txt");
        fs::write(&file, b"payload").expect("write file");

        // Blocking receive with a generous ceiling: the event arrives at
        // FSEvents latency (typically well under a second); only a real
        // platform-watcher → bridge → recorder wiring bug exhausts it.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = rx
                .recv_timeout(remaining)
                .expect("watch event must reach the recorder");
            if event.path == file {
                break;
            }
        }
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
