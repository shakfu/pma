//! The shipped agents, recipes and kits, embedded at build time.
//!
//! Recipes and kits are read from a real directory: a build context copies files from it, and a
//! `copy` section is checked against its path. So the embedded files are written out to the cache
//! directory, under a hash of their content, and checked against the binary on every use: a file
//! edited there is written back rather than trusted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use ring::digest::{Context, SHA256, digest};

use crate::error::Result;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/resources.rs"));
}

pub use embedded::FILES;

/// The directory under `cache` holding the shipped `agents/`, `recipes/` and `kits/`. Extracted
/// once per cache directory per process.
pub fn root(cache: &Path) -> Result<PathBuf> {
    static ROOTS: Mutex<BTreeMap<PathBuf, std::result::Result<PathBuf, String>>> =
        Mutex::new(BTreeMap::new());
    let mut roots = ROOTS.lock().unwrap_or_else(PoisonError::into_inner);
    roots
        .entry(cache.to_path_buf())
        .or_insert_with(|| extract(cache).map_err(|e| e.message))
        .clone()
        .map_err(crate::error::Error::new)
}

fn extract(cache: &Path) -> Result<PathBuf> {
    let mut hash = Context::new(&SHA256);
    for (rel, data) in FILES {
        hash.update(rel.as_bytes());
        hash.update(&[0]);
        hash.update(digest(&SHA256, data).as_ref());
    }
    let digest = crate::util::hex(hash.finish().as_ref());
    let dir = cache.join("resources").join(&digest[..16]);
    for (rel, data) in FILES {
        let path = dir.join(rel);
        if std::fs::read(&path).is_ok_and(|on_disk| on_disk == *data) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Written beside, then renamed: two processes extracting at once never read half a file.
        let partial = path.with_extension(format!("partial-{}", std::process::id()));
        std::fs::write(&partial, data)?;
        std::fs::rename(&partial, &path)?;
    }
    Ok(dir)
}
