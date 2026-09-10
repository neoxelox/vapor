//! Linux `FsWatcher`: inotify through the shared `notify` backend. The
//! backend registers one watch per directory, so a large tree spends
//! the user's `fs.inotify.max_user_watches`; running out fails the
//! start with the sysctl to raise, and a full kernel queue reaches the
//! engine as an `Other` event on the root (a whole-scope reconcile).

pub use super::notify_backend::NativeFsWatcher;
