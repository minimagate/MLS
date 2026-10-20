//! Shared gate for the real TeXmacs checks. They run only when `MLS_TEXMACS_TESTS=1`. Otherwise,
//! or when the binary at `MLS_TEXMACS_BIN` (default: the app bundle) does not exist, they skip
//! with a message and pass.

use std::path::PathBuf;

const DEFAULT_BIN: &str = "/Applications/TeXmacs.app/Contents/MacOS/TeXmacs";

/// The TeXmacs binary, when the real checks should run. `None` means the test should return.
pub fn gated_bin() -> Option<PathBuf> {
    if std::env::var("MLS_TEXMACS_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_TEXMACS_TESTS=1 to run real TeXmacs checks");
        return None;
    }
    let bin = std::env::var_os("MLS_TEXMACS_BIN").map_or_else(|| DEFAULT_BIN.into(), PathBuf::from);
    if !bin.exists() {
        eprintln!("skipped: TeXmacs binary not found at {}", bin.display());
        return None;
    }
    Some(bin)
}
