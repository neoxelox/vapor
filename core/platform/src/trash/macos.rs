//! macOS `TrashBin`: `NSFileManager.trashItem(at:resultingItemURL:)`,
//! the call the Finder itself is built on. The system picks the trash
//! of the volume the item is on (`~/.Trash` on the home volume,
//! `.Trashes/<uid>` on another one), numbers a name that is taken, and
//! records where the item came from, so the Finder offers Put Back.
//!
//! A rename into `~/.Trash` did none of that: it failed for a file on
//! another volume, and the Finder had no place to put the file back to.
//!
//! The call goes through the Objective-C runtime. The FFI is the five
//! symbols below, and each `unsafe` block states what it relies on.
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_void};
use std::io;
use std::path::{Path, PathBuf};
use std::ptr;

use core_foundation::base::TCFType;
use core_foundation::error::{CFError, CFErrorRef};
use core_foundation::url::{CFURL, CFURLRef};

use super::TrashBin;

type Object = *mut c_void;
type Selector = *mut c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Object;
    fn sel_registerName(name: *const c_char) -> Selector;
    fn objc_msgSend();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

// `NSFileManager` lives in Foundation; the link makes the class known
// to the runtime.
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[derive(Debug, Default)]
pub struct NativeTrashBin;

impl NativeTrashBin {
    pub fn for_current_host() -> Self {
        Self
    }
}

impl TrashBin for NativeTrashBin {
    fn trash(&self, path: &Path) -> io::Result<PathBuf> {
        let metadata = std::fs::symlink_metadata(path)?;
        let url = CFURL::from_path(path, metadata.is_dir()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a path the system can name", path.display()),
            )
        })?;
        trash_item(&url)
    }
}

/// Sends `trashItemAtURL:resultingItemURL:error:` to the default file
/// manager and returns where the item landed.
fn trash_item(url: &CFURL) -> io::Result<PathBuf> {
    const FILE_MANAGER: &CStr = c"NSFileManager";
    const DEFAULT_MANAGER: &CStr = c"defaultManager";
    const TRASH_ITEM: &CStr = c"trashItemAtURL:resultingItemURL:error:";

    // SAFETY: the names are NUL-terminated constants. `objc_getClass`
    // and `sel_registerName` only read them; a class the runtime does
    // not know comes back null and is checked before use.
    let (class, default_manager, trash_item) = unsafe {
        (
            objc_getClass(FILE_MANAGER.as_ptr()),
            sel_registerName(DEFAULT_MANAGER.as_ptr()),
            sel_registerName(TRASH_ITEM.as_ptr()),
        )
    };
    if class.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this host has no file manager to trash an item with",
        ));
    }

    // SAFETY: `objc_msgSend` has no signature of its own; it is called
    // through a pointer of the exact type of the method it sends. Both
    // types below are the documented signatures of the two methods
    // (`+defaultManager` returns an object; `-trashItemAtURL:…` takes an
    // `NSURL *` and two out-pointers and returns `BOOL`, one byte).
    // `NSURL` and `NSError` are toll-free bridged with `CFURL` and
    // `CFError`. The out-pointers are valid for the call. What the call
    // hands back is autoreleased: it is retained (`wrap_under_get_rule`)
    // before the pool that owns it is popped, and the pool is popped on
    // every path.
    unsafe {
        let send_default: unsafe extern "C" fn(Object, Selector) -> Object =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_trash: unsafe extern "C" fn(
            Object,
            Selector,
            CFURLRef,
            *mut CFURLRef,
            *mut CFErrorRef,
        ) -> u8 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        let pool = objc_autoreleasePoolPush();
        let manager = send_default(class, default_manager);
        let mut landed: CFURLRef = ptr::null();
        let mut error: CFErrorRef = ptr::null_mut();
        let moved = !manager.is_null()
            && send_trash(
                manager,
                trash_item,
                url.as_concrete_TypeRef(),
                &mut landed,
                &mut error,
            ) != 0;
        let outcome = if moved && !landed.is_null() {
            CFURL::wrap_under_get_rule(landed).to_path().ok_or_else(|| {
                io::Error::other("the system moved the item to the trash and gave no path for it")
            })
        } else if !error.is_null() {
            Err(io::Error::other(
                CFError::wrap_under_get_rule(error)
                    .description()
                    .to_string(),
            ))
        } else {
            Err(io::Error::other("the system refused to trash the item"))
        };
        objc_autoreleasePoolPop(pool);
        outcome
    }
}
