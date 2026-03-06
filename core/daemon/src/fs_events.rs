use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use notify::event::{CreateKind, ModifyKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::logging;
use crate::path_filter::{EventPathFilter, EventPathFilterOptions};

#[derive(Clone, Debug, Eq, PartialEq)]
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
            Self::Notify(error) => write!(f, "notify watcher error: {error}"),
        }
    }
}

impl Error for FsEventsWatcherError {}

impl From<notify::Error> for FsEventsWatcherError {
    fn from(value: notify::Error) -> Self {
        Self::Notify(value)
    }
}

pub struct FsEventsWatcher {
    _watcher: RecommendedWatcher,
    watch_root: PathBuf,
}

impl FsEventsWatcher {
    pub fn start(
        watch_root: impl Into<PathBuf>,
        recorder: Arc<dyn FsEventRecording>,
    ) -> Result<Self, FsEventsWatcherError> {
        let watch_root = normalize_watch_root(watch_root.into())?;
        let callback_watch_root = watch_root.clone();
        let callback_recorder = Arc::clone(&recorder);
        let callback_path_filter = Arc::new(EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions::from_process_environment(),
        ));

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
        })
    }

    pub fn watch_root(&self) -> &Path {
        &self.watch_root
    }
}

fn normalize_watch_root(watch_root: PathBuf) -> Result<PathBuf, FsEventsWatcherError> {
    if watch_root.as_os_str().is_empty() {
        return Err(FsEventsWatcherError::EmptyWatchRoot);
    }

    if !watch_root.exists() {
        return Err(FsEventsWatcherError::WatchRootMissing(watch_root));
    }

    if !watch_root.is_dir() {
        return Err(FsEventsWatcherError::WatchRootNotDirectory(watch_root));
    }

    Ok(watch_root)
}

fn record_callback_result(
    watch_root: &Path,
    path_filter: &EventPathFilter,
    result: Result<Event, notify::Error>,
    recorder: &dyn FsEventRecording,
) {
    match result {
        Ok(event) => {
            let kind = map_event_kind(&event.kind);
            for path in event.paths {
                if let Some(path) = normalize_event_path(watch_root, &path) {
                    if path_filter.should_ignore(&path) {
                        continue;
                    }

                    recorder.record_event(FsEventRecord {
                        path,
                        kind: kind.clone(),
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

fn normalize_event_path(watch_root: &Path, event_path: &Path) -> Option<PathBuf> {
    let candidate = if event_path.is_absolute() {
        event_path.to_path_buf()
    } else {
        watch_root.join(event_path)
    };

    if candidate.starts_with(watch_root) {
        Some(candidate)
    } else {
        None
    }
}

fn map_event_kind(kind: &EventKind) -> FsEventKind {
    match kind {
        EventKind::Create(CreateKind::Any)
        | EventKind::Create(CreateKind::File)
        | EventKind::Create(CreateKind::Folder)
        | EventKind::Create(CreateKind::Other) => FsEventKind::Created,
        EventKind::Modify(ModifyKind::Name(_)) => FsEventKind::Renamed,
        EventKind::Modify(_) => FsEventKind::Modified,
        EventKind::Remove(_) => FsEventKind::Removed,
        _ => FsEventKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn callback_normalizes_relative_paths_and_records_metadata() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
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
        assert_eq!(events[0].path, PathBuf::from("/tmp/vapor-root/src/main.rs"));
    }

    #[test]
    fn callback_filters_paths_outside_watch_root() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: vec![PathBuf::from("/tmp/other-root/file.txt")],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert!(events.is_empty());
    }

    #[test]
    fn callback_maps_rename_events() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
        let path_filter = test_path_filter(&watch_root);
        let recorder = TestRecorder::default();

        let event = Event {
            kind: EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::Any)),
            paths: vec![PathBuf::from("/tmp/vapor-root/renamed.txt")],
            attrs: Default::default(),
        };

        record_callback_result(&watch_root, &path_filter, Ok(event), &recorder);

        let events = recorder.events.lock().expect("events mutex poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, FsEventKind::Renamed);
    }

    #[test]
    fn callback_records_notify_errors() {
        let watch_root = PathBuf::from("/tmp/vapor-root");
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
        let watch_root = PathBuf::from("/tmp/vapor-root");
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

    fn test_path_filter(watch_root: &Path) -> EventPathFilter {
        let options = EventPathFilterOptions {
            use_gitignore: false,
            use_vaporignore: false,
            ..EventPathFilterOptions::default()
        };
        EventPathFilter::for_watch_root(watch_root, &options)
    }
}
