//! The name of a keep-both conflict copy. The daemon and the providers
//! both make copies, and a copy is found again by its name, so the
//! template has one owner.

use crate::constants::conflict::COPY_MARKER;

/// The name of a copy of `file_name`:
/// `{stem}~conflict-{device_id}-{timestamp_ms}[-{sequence}]{ext}`. The
/// split is at the last `.` of the name; a leading dot (a hidden file)
/// is part of the stem. `sequence` tells apart two copies that would
/// have one name.
pub fn copy_name(
    file_name: &str,
    device_id: &str,
    timestamp_ms: u64,
    sequence: Option<u64>,
) -> String {
    let (stem, extension) = match file_name.rfind('.') {
        Some(0) | None => (file_name, ""),
        Some(index) => file_name.split_at(index),
    };
    match sequence {
        None => format!("{stem}{COPY_MARKER}{device_id}-{timestamp_ms}{extension}"),
        Some(sequence) => {
            format!("{stem}{COPY_MARKER}{device_id}-{timestamp_ms}-{sequence}{extension}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copy_name_keeps_the_extension_and_the_hidden_dot() {
        assert_eq!(
            copy_name("report.final.md", "mbp-x7k2", 1_763_900_000_000, None),
            "report.final~conflict-mbp-x7k2-1763900000000.md"
        );
        assert_eq!(
            copy_name(".env", "replaced", 1_763_900_000_000, Some(2)),
            ".env~conflict-replaced-1763900000000-2"
        );
    }
}
