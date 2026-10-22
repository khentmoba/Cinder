use crate::{Artifact, LimitRow, Row};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CachedLimit {
    pub key: String,
    pub timestamp: String,
    pub limit: LimitRow,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CachedFile {
    pub mtime_ns: u128,
    pub size: u64,
    pub rows: Vec<Row>,
    pub sessions: Vec<(String, String)>,
    pub plan_routes: Vec<(String, u64)>,
    pub limits: Vec<CachedLimit>,
    pub thread_id: Option<String>,
    /// Artifacts the file published, kept so a cached file can hand them back.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CachedDb {
    pub mtime_ns: u128,
    pub size: u64,
    /// The stamp of the write-ahead log beside the database.
    ///
    /// A database in write-ahead mode keeps the newest writes in that log
    /// until it is checkpointed, so the file alone does not say when the data
    /// last changed.
    #[serde(default)]
    pub wal: (u128, u64),
    pub count: u64,
    pub rows: Vec<Row>,
    pub sessions: Vec<(String, String)>,
}

/// The version of the row format the cache holds.
///
/// A change to the shape or the meaning of a row bumps it, so a cache written
/// by an older build is rebuilt instead of being replayed with values that no
/// longer mean what they say.
pub const FORMAT_VERSION: u32 = 4;

#[derive(Serialize, Deserialize, Default)]
pub struct ScanCache {
    pub pricing_fingerprint: u64,
    /// The format version this cache was written with.
    #[serde(default)]
    pub format_version: u32,
    pub files: HashMap<String, CachedFile>,
    pub opencode_db: Option<CachedDb>,
    pub t3_state_db: Option<CachedDb>,
    #[serde(skip)]
    pub cache_path: PathBuf,
    #[serde(skip)]
    pub dirty: bool,
}

pub fn file_stamp(path: &Path) -> Option<(u128, u64)> {
    let m = fs::metadata(path).ok()?;
    let size = m.len();
    let mtime = m.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((dur.as_nanos(), size))
}

pub fn cache_file_for(home: &Path) -> PathBuf {
    home.join(".cinder").join("scan_cache.json")
}

impl ScanCache {
    pub fn load_for(home: &Path, pricing_fp: u64) -> Self {
        let p = cache_file_for(home);
        if let Ok(file) = fs::File::open(&p) {
            let reader = std::io::BufReader::with_capacity(128 * 1024, file);
            if let Ok(mut c) = serde_json::from_reader::<_, ScanCache>(reader) {
                if c.pricing_fingerprint == pricing_fp && c.format_version == FORMAT_VERSION {
                    c.cache_path = p;
                    c.dirty = false;
                    return c;
                }
            }
        }
        ScanCache {
            pricing_fingerprint: pricing_fp,
            format_version: FORMAT_VERSION,
            cache_path: p,
            dirty: true,
            ..Default::default()
        }
    }

    pub fn get_file(&self, path_str: &str, mtime_ns: u128, size: u64) -> Option<&CachedFile> {
        let entry = self.files.get(path_str)?;
        if entry.mtime_ns == mtime_ns && entry.size == size {
            Some(entry)
        } else {
            None
        }
    }

    pub fn save(&mut self) {
        if !self.dirty || self.cache_path.as_os_str().is_empty() {
            return;
        }
        if let Some(parent) = self.cache_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let temp_path = self.cache_path.with_extension("tmp");
        if let Ok(file) = fs::File::create(&temp_path) {
            let writer = std::io::BufWriter::with_capacity(128 * 1024, file);
            if serde_json::to_writer(writer, self).is_ok() {
                let _ = fs::rename(&temp_path, &self.cache_path);
                self.dirty = false;
            } else {
                let _ = fs::remove_file(&temp_path);
            }
        }
    }

    pub fn clear(home: &Path) {
        let path = cache_file_for(home);
        let _ = fs::remove_file(path);
    }
}
