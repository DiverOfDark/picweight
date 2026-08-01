//! On-disk assets. Exactly one kind exists: the 768px meal thumbnail.
//!
//! PRD §7 is emphatic that originals are **not** retained. Ingest writes the
//! upload to a temp file, [`thumbs::store_from_bytes`] produces the single
//! derivative, and the original is deleted. The thumbnail is simultaneously the
//! display asset and a valid re-analysis input, which is why it is 768px rather
//! than 320px — correction-by-conversation (§5) re-attaches it from disk.

pub mod thumbs;

use crate::config::Config;
use crate::error::AppError;
use std::path::PathBuf;

/// Root of the content-addressed thumbnail tree: `<data_path>/thumbs`.
pub fn thumbs_root(config: &Config) -> PathBuf {
    config.thumbs_root()
}

/// Create every directory the process writes to. Idempotent.
pub fn ensure_dirs(config: &Config) -> Result<(), AppError> {
    std::fs::create_dir_all(thumbs_root(config))?;
    Ok(())
}
