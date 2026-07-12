//! Validated remote-path newtype.
//!
//! Every path the engine hands a provider is relative to the cloud sync
//! root, forward-slash separated, and free of traversal components.
//! Validation happens once at construction so provider implementations
//! can trust the invariant instead of re-checking at every call site
//! (strict scope enforcement).

use std::error::Error;
use std::fmt::{self, Display};
use std::path::{Component, Path, PathBuf};

/// A path relative to the provider's cloud sync root.
///
/// Invariants (enforced at construction):
/// - Separators are `/` regardless of host OS.
/// - No empty segments, no `.` or `..` segments, no leading `/`.
/// - Not empty except for the dedicated [`RemotePath::root`] value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemotePath(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemotePathError {
    Empty,
    Absolute(String),
    Traversal(String),
    EmptySegment(String),
    /// A literal backslash appears in the path. `/` is the one canonical
    /// separator; a `\` is ambiguous (a legal filename character on
    /// macOS/Linux, the separator on Windows) and would round-trip to a
    /// different file across OSes, so it is refused until an escaping
    /// scheme exists rather than silently remapped to a nested path.
    Backslash(String),
}

impl Display for RemotePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "remote path is empty"),
            Self::Absolute(path) => write!(f, "remote path must be relative: {path}"),
            Self::Traversal(path) => {
                write!(f, "remote path contains traversal segments: {path}")
            }
            Self::EmptySegment(path) => write!(f, "remote path contains empty segments: {path}"),
            Self::Backslash(path) => {
                write!(f, "remote path contains an unsupported backslash: {path}")
            }
        }
    }
}

impl Error for RemotePathError {}

impl RemotePath {
    /// The cloud sync root itself.
    pub fn root() -> Self {
        Self(String::new())
    }

    pub fn new(raw: impl Into<String>) -> Result<Self, RemotePathError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(RemotePathError::Empty);
        }
        if raw.starts_with('/') {
            return Err(RemotePathError::Absolute(raw));
        }
        // `\` is not a separator here (that translation belongs at the
        // Windows-native boundary in `from_local`); a literal backslash in
        // a name is refused rather than remapped into a nested path.
        if raw.contains('\\') {
            return Err(RemotePathError::Backslash(raw));
        }
        for segment in raw.split('/') {
            if segment.is_empty() {
                return Err(RemotePathError::EmptySegment(raw));
            }
            if segment == "." || segment == ".." {
                return Err(RemotePathError::Traversal(raw));
            }
        }
        Ok(Self(raw))
    }

    /// Derives the remote path of `absolute` relative to `local_root`.
    /// `None` when `absolute` is not inside `local_root` or is not
    /// valid UTF-8 (the durable queue already enforces UTF-8 paths).
    pub fn from_local(local_root: &Path, absolute: &Path) -> Option<Self> {
        let relative = absolute.strip_prefix(local_root).ok()?;
        let mut segments: Vec<&str> = Vec::new();
        for component in relative.components() {
            match component {
                Component::Normal(segment) => segments.push(segment.to_str()?),
                Component::CurDir => {}
                _ => return None,
            }
        }
        if segments.is_empty() {
            return None;
        }
        Self::new(segments.join("/")).ok()
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The final path segment (file name); `None` for the root.
    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            return None;
        }
        self.0.rsplit('/').next()
    }

    /// The parent remote path; `None` when `self` is the root.
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        match self.0.rsplit_once('/') {
            Some((parent, _)) => Some(Self(parent.to_string())),
            None => Some(Self::root()),
        }
    }

    /// Appends exactly one path segment. A `segment` that itself contains
    /// a separator (`/` or `\`) is rejected rather than silently expanded
    /// into multiple levels — a Drive object legally named `a/b` must not
    /// become the nested path `a` → `b`.
    pub fn join(&self, segment: &str) -> Result<Self, RemotePathError> {
        if segment.contains('/') {
            return Err(RemotePathError::EmptySegment(segment.to_string()));
        }
        if segment.contains('\\') {
            return Err(RemotePathError::Backslash(segment.to_string()));
        }
        if self.is_root() {
            Self::new(segment)
        } else {
            Self::new(format!("{}/{segment}", self.0))
        }
    }

    /// Resolves this remote path under a filesystem root using native
    /// separators. Purely lexical; scope safety additionally requires
    /// the symlink checks in the filesystem provider.
    pub fn resolve_under(&self, root: &Path) -> PathBuf {
        if self.is_root() {
            return root.to_path_buf();
        }
        let mut resolved = root.to_path_buf();
        for segment in self.0.split('/') {
            resolved.push(segment);
        }
        resolved
    }

    /// Maps this remote path to the equivalent absolute local path
    /// under `local_root` (the mirror of [`RemotePath::from_local`]).
    pub fn to_local(&self, local_root: &Path) -> PathBuf {
        self.resolve_under(local_root)
    }
}

impl Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_root() {
            write!(f, "<root>")
        } else {
            write!(f, "{}", self.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_nested_relative_paths() {
        let path = RemotePath::new("docs/notes/today.md").expect("valid path");
        assert_eq!(path.as_str(), "docs/notes/today.md");
        assert_eq!(path.file_name(), Some("today.md"));
        assert_eq!(path.parent(), Some(RemotePath::new("docs/notes").unwrap()));
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        assert_eq!(
            RemotePath::new("../escape.txt"),
            Err(RemotePathError::Traversal("../escape.txt".to_string()))
        );
        assert_eq!(
            RemotePath::new("docs/../escape.txt"),
            Err(RemotePathError::Traversal("docs/../escape.txt".to_string()))
        );
        assert_eq!(
            RemotePath::new("/etc/passwd"),
            Err(RemotePathError::Absolute("/etc/passwd".to_string()))
        );
        assert_eq!(RemotePath::new(""), Err(RemotePathError::Empty));
        assert_eq!(
            RemotePath::new("docs//double.txt"),
            Err(RemotePathError::EmptySegment(
                "docs//double.txt".to_string()
            ))
        );
    }

    #[test]
    fn backslash_is_refused_not_remapped_to_a_nested_path() {
        // A macOS/Linux file legally named `foo\bar.txt` is one segment; it
        // must not be silently remapped into a nested `foo` → `bar.txt`.
        assert_eq!(
            RemotePath::new("foo\\bar.txt"),
            Err(RemotePathError::Backslash("foo\\bar.txt".to_string()))
        );
        assert_eq!(
            RemotePath::new("docs\\notes\\today.md"),
            Err(RemotePathError::Backslash(
                "docs\\notes\\today.md".to_string()
            ))
        );
    }

    #[test]
    fn join_refuses_multi_segment_names() {
        // A Drive object legally named `a/b` must not expand into two levels.
        let docs = RemotePath::new("docs").expect("valid");
        assert!(docs.join("a/b").is_err());
        assert!(docs.join("a\\b").is_err());
        assert_eq!(
            docs.join("plain.txt").expect("single").as_str(),
            "docs/plain.txt"
        );
    }

    #[test]
    fn from_local_derives_relative_path_inside_root() {
        let root = Path::new("/tmp/watch");
        let path =
            RemotePath::from_local(root, Path::new("/tmp/watch/docs/a.txt")).expect("inside root");
        assert_eq!(path.as_str(), "docs/a.txt");
        assert_eq!(path.to_local(root), PathBuf::from("/tmp/watch/docs/a.txt"));
    }

    #[test]
    fn from_local_rejects_paths_outside_root_and_the_root_itself() {
        let root = Path::new("/tmp/watch");
        assert_eq!(
            RemotePath::from_local(root, Path::new("/tmp/elsewhere/a.txt")),
            None
        );
        assert_eq!(RemotePath::from_local(root, root), None);
    }

    #[test]
    fn root_round_trips_through_resolve() {
        let root = RemotePath::root();
        assert!(root.is_root());
        assert_eq!(root.resolve_under(Path::new("/cloud")), Path::new("/cloud"));
        assert_eq!(root.parent(), None);
        assert_eq!(root.file_name(), None);
    }

    #[test]
    fn join_extends_paths_from_root_and_nested() {
        let joined = RemotePath::root().join("docs").expect("join from root");
        assert_eq!(joined.as_str(), "docs");
        let nested = joined.join("a.txt").expect("join nested");
        assert_eq!(nested.as_str(), "docs/a.txt");
        assert!(joined.join("..").is_err());
    }
}
