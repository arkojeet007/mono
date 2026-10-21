// SPDX-License-Identifier: MPL-2.0

//! Zip recent application logs for an opt-in feedback attachment.
//!
//! The files themselves are read by the submit worker. This module only packs
//! already-loaded bytes so tests never touch the store log directory.

use std::io::{Cursor, Write};

use zip::ZipWriter;
use zip::write::SimpleFileOptions;

/// Uncompressed bytes we are willing to pack. Oldest files are dropped first.
pub const MAX_UNCOMPRESSED: usize = 2 * 1024 * 1024;
/// Zip size we are willing to attach. Vercel caps the whole request at 4.5 MB.
pub const MAX_ZIP: usize = 1536 * 1024;

/// Pack `files` (name, bytes) into a zip, dropping oldest names until both
/// size caps hold. Names are sorted so dated rotation files (`koru.log.2026-09-01`)
/// go before the live `koru.log`.
pub fn zip_logs(mut files: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, String> {
    files.sort_by(|a, b| age_key(&a.0).cmp(&age_key(&b.0)));
    while !files.is_empty() {
        let uncompressed: usize = files.iter().map(|(_, b)| b.len()).sum();
        if uncompressed > MAX_UNCOMPRESSED {
            files.remove(0);
            continue;
        }
        let zip = write(&files)?;
        if zip.len() <= MAX_ZIP {
            return Ok(zip);
        }
        files.remove(0);
    }
    Err("logs are too large to attach".into())
}

fn write(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for (name, bytes) in files {
        let name = basename(name);
        if name.is_empty() {
            continue;
        }
        archive
            .start_file(name, options)
            .map_err(|error| error.to_string())?;
        archive
            .write_all(bytes)
            .map_err(|error| error.to_string())?;
    }
    let cursor = archive.finish().map_err(|error| error.to_string())?;
    Ok(cursor.into_inner())
}

/// Live `*.log` files sort after dated rotations so the oldest rotation is
/// dropped first and today's log is the last to go.
fn age_key(name: &str) -> (u8, &str) {
    let base = basename(name);
    if base.ends_with(".log") {
        (1, base)
    } else {
        (0, base)
    }
}

fn basename(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zip_contains_the_named_entries() {
        let zip = zip_logs(vec![
            ("koru.log".into(), b"hello".to_vec()),
            ("koru-agent.log".into(), b"agent".to_vec()),
        ])
        .expect("zip");
        let names = list(&zip);
        assert!(names.iter().any(|n| n == "koru.log"));
        assert!(names.iter().any(|n| n == "koru-agent.log"));
    }

    #[test]
    fn oversized_sets_drop_the_oldest_name() {
        let big = vec![0u8; MAX_UNCOMPRESSED / 2 + 16];
        let zip = zip_logs(vec![
            ("koru.log.2026-01-01".into(), big.clone()),
            ("koru.log.2026-01-02".into(), big.clone()),
            ("koru.log".into(), b"live".to_vec()),
        ])
        .expect("zip after dropping");
        let names = list(&zip);
        assert!(
            !names.iter().any(|n| n == "koru.log.2026-01-01"),
            "the oldest dated file is the one that goes"
        );
        assert!(names.iter().any(|n| n == "koru.log"));
    }

    #[test]
    fn a_path_is_stored_as_its_basename() {
        let zip = zip_logs(vec![("C:\\logs\\koru.log".into(), b"x".to_vec())]).expect("zip");
        assert_eq!(list(&zip), vec!["koru.log"]);
    }

    fn list(bytes: &[u8]) -> Vec<String> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("archive");
        (0..archive.len())
            .map(|i| {
                let entry = archive.by_index(i).expect("entry");
                entry.name().expect("name").into_owned()
            })
            .collect()
    }
}
