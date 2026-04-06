// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! Persistence — each history record is appended to a JSONL file the moment
//! it is stored, so no data can be lost between periodic flushes.
//!
//! File layout: one `PersistedRecord` JSON object per line (JSONL).
//! On startup the file is read into the store and then compacted (rewritten)
//! to remove records that have aged out of the retention window.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::store::{PersistedRecord, Store};

/// Load previously saved records into `store` and compact the file.
/// If the file does not exist the store is left empty (fresh start).
pub fn load(store: &Store, path: &Path) {
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("persist: no data file at {path:?} — starting fresh");
            return;
        }
        Err(e) => {
            warn!("persist: failed to read {path:?}: {e}");
            return;
        }
    };

    let mut records: Vec<PersistedRecord> = Vec::new();
    let mut bad = 0usize;
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<PersistedRecord>(line) {
            Ok(r) => records.push(r),
            Err(_) => bad += 1,
        }
    }
    if bad > 0 {
        warn!("persist: skipped {bad} malformed lines in {path:?}");
    }

    let n = store.restore(records);
    info!("persist: loaded {n} records from {path:?}");

    // Compact: rewrite the file with only the in-retention records now in the store.
    rewrite(store, path);
}

/// Append a single record to the JSONL file.
pub fn append(record: &PersistedRecord, path: &Path) {
    let mut line = match serde_json::to_string(record) {
        Ok(s) => s,
        Err(e) => {
            warn!("persist: serialise failed: {e}");
            return;
        }
    };
    line.push('\n');

    let mut file = match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(e) => {
            warn!("persist: append open failed: {e}");
            return;
        }
    };
    if let Err(e) = file.write_all(line.as_bytes()) {
        warn!("persist: append write failed: {e}");
    }
}

/// Truncate the data file after a history clear.
pub fn clear(path: &Path) {
    if let Err(e) = std::fs::write(path, b"") {
        warn!("persist: clear failed: {e}");
    }
}

/// Rewrite the file atomically with only the records currently in the store.
fn rewrite(store: &Store, path: &Path) {
    let records = store.current_records();
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        for record in &records {
            if let Ok(mut line) = serde_json::to_string(record) {
                line.push('\n');
                file.write_all(line.as_bytes())?;
            }
        }
        file.flush()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    match result {
        Ok(()) => info!("persist: compacted to {} records in {path:?}", records.len()),
        Err(e) => warn!("persist: rewrite failed: {e}"),
    }
}
