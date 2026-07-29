//! Shared SQLite location for `control-plane-admin`, `sag-policy`, and any other service using [`crate::SqliteStore`].
//!
//! Override with environment variable **`SAG_STORAGE_DB_PATH`** (absolute or relative).
//! When unset, all services default to the **same relative path** so they open one file **as long as
//! every process uses the same current directory** — run `cargo run -p …` from the **`sag-cloud`**
//! crate root (same rule on Windows and WSL when the repo is on a shared mount, e.g. `/mnt/d/...` vs `D:\...`).

use std::path::Path;

/// Default relative path (relative to process current working directory).
pub const DEFAULT_STORAGE_DB_REL_PATH: &str = "data/sag-storage/sag.db";

/// Resolves the SQLite file path: `SAG_STORAGE_DB_PATH` if set, else [`DEFAULT_STORAGE_DB_REL_PATH`].
#[must_use]
pub fn resolve_storage_db_path() -> String {
    std::env::var("SAG_STORAGE_DB_PATH").unwrap_or_else(|_| DEFAULT_STORAGE_DB_REL_PATH.to_string())
}

/// Ensures the parent directory of `db_path` exists (idempotent).
pub fn ensure_storage_dir_for_path(db_path: &str) {
    if let Some(parent) = Path::new(db_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
}
