// SPDX-License-Identifier: MPL-2.0

//! TextBundle v2 package export, with TextPack as its compressed variant.
//!
//! Markdown owns semantic rendering. This crate owns only the container:
//! `info.json`, `text.markdown`, and the `assets/` paths required by the
//! TextBundle specification.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use koru_export::{
    BlobSource, ExportDocument, ExportError, ExportProgress, ExportResult, ExportStage, Exporter,
    ExporterDescriptor, unique_path,
};
use koru_export_markdown::{Rendered, render_document};
use zip::write::SimpleFileOptions;

pub static DESCRIPTOR: ExporterDescriptor = ExporterDescriptor {
    id: "textbundle",
    name: "TextBundle",
    extensions: &["textbundle", "textpack", "zip"],
};

pub struct TextBundleExporter;

impl Exporter for TextBundleExporter {
    fn descriptor(&self) -> &'static ExporterDescriptor {
        &DESCRIPTOR
    }

    fn export(
        &self,
        documents: &[ExportDocument],
        destination: &Path,
        blobs: &dyn BlobSource,
        progress: &dyn ExportProgress,
    ) -> Result<ExportResult, ExportError> {
        if documents.is_empty() {
            return Err(ExportError::new("nothing to export"));
        }
        if progress.cancelled() {
            return Err(ExportError::cancelled());
        }

        let multi = documents.len() > 1;
        if multi && !destination.is_dir() {
            return Err(ExportError::new(format!(
                "multi-document TextBundle export needs a directory, got {}",
                destination.display()
            )));
        }

        let total = documents.len();
        let mut files = Vec::with_capacity(total);
        for (index, document) in documents.iter().enumerate() {
            if progress.cancelled() {
                return Err(ExportError::cancelled());
            }
            progress.update(ExportStage::Converting, index, Some(total));
            let rendered = render_document(&document.doc, blobs, "assets")?;
            let path = if multi {
                let name = koru_export::suggested_file_name(&document.title, &["textbundle"]);
                unique_path(destination, &name)
            } else {
                destination.to_path_buf()
            };

            progress.update(ExportStage::Writing, index, Some(total));
            if has_extension(&path, "textpack") || has_extension(&path, "zip") {
                write_textpack(&path, &rendered)?;
            } else {
                write_textbundle(&path, &rendered, progress, index, total)?;
            }
            files.push(path);
        }
        progress.update(ExportStage::Writing, total, Some(total));

        let remembered_dir = if multi {
            destination.to_path_buf()
        } else {
            destination
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        };
        Ok(ExportResult {
            files,
            remembered_dir,
        })
    }
}

fn has_extension(path: &Path, wanted: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(wanted))
}

fn metadata() -> Result<Vec<u8>, ExportError> {
    let mut bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "version": 2,
        "type": "net.daringfireball.markdown",
        "transient": false,
        "creatorIdentifier": "do.koru.desktop"
    }))
    .map_err(|error| ExportError::new(error.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_textbundle(
    destination: &Path,
    rendered: &Rendered,
    progress: &dyn ExportProgress,
    index: usize,
    total: usize,
) -> Result<(), ExportError> {
    ensure_parent(destination)?;
    let staging = side_path(destination, "part");
    let result = (|| {
        fs::create_dir(&staging).map_err(failed)?;
        write_synced(&staging.join("info.json"), &metadata()?)?;
        write_synced(&staging.join("text.markdown"), rendered.markdown.as_bytes())?;
        if !rendered.assets.is_empty() {
            progress.update(ExportStage::WritingMedia, index, Some(total));
            let assets = staging.join("assets");
            fs::create_dir(&assets).map_err(failed)?;
            for asset in &rendered.assets {
                let name = Path::new(&asset.file_name)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| ExportError::new("Markdown produced an invalid asset name"))?;
                write_synced(&assets.join(name), &asset.bytes)?;
            }
        }
        replace_path(&staging, destination)
    })();
    if result.is_err() {
        let _ = remove_path(&staging);
    }
    result
}

fn write_textpack(destination: &Path, rendered: &Rendered) -> Result<(), ExportError> {
    ensure_parent(destination)?;
    let staging = side_path(destination, "part");
    let result = (|| {
        let file = File::create(&staging).map_err(failed)?;
        let mut archive = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        archive.start_file("info.json", options).map_err(failed)?;
        archive.write_all(&metadata()?).map_err(failed)?;
        archive
            .start_file("text.markdown", options)
            .map_err(failed)?;
        archive
            .write_all(rendered.markdown.as_bytes())
            .map_err(failed)?;
        for asset in &rendered.assets {
            let name = Path::new(&asset.file_name)
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| ExportError::new("Markdown produced an invalid asset name"))?;
            archive
                .start_file(format!("assets/{name}"), options)
                .map_err(failed)?;
            archive.write_all(&asset.bytes).map_err(failed)?;
        }
        let file = archive.finish().map_err(failed)?;
        file.sync_all().map_err(failed)?;
        replace_path(&staging, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

fn ensure_parent(path: &Path) -> Result<(), ExportError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(failed)?;
    }
    Ok(())
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), ExportError> {
    let mut file = File::create(path).map_err(failed)?;
    file.write_all(bytes).map_err(failed)?;
    file.sync_all().map_err(failed)
}

static NEXT_SIDE_PATH: AtomicU64 = AtomicU64::new(0);

fn side_path(destination: &Path, suffix: &str) -> PathBuf {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document.textbundle");
    let serial = NEXT_SIDE_PATH.fetch_add(1, Ordering::Relaxed);
    destination.with_file_name(format!(
        ".{name}.{}.{}.{}",
        std::process::id(),
        serial,
        suffix
    ))
}

/// Replace a package or compressed file without first deleting the old value.
/// If the second rename fails, the old export is put back.
fn replace_path(staging: &Path, destination: &Path) -> Result<(), ExportError> {
    if !destination.exists() {
        return fs::rename(staging, destination).map_err(failed);
    }
    let previous = side_path(destination, "previous");
    fs::rename(destination, &previous).map_err(failed)?;
    match fs::rename(staging, destination) {
        Ok(()) => {
            let _ = remove_path(&previous);
            Ok(())
        }
        Err(error) => {
            let _ = fs::rename(&previous, destination);
            Err(failed(error))
        }
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else if path.exists() {
        fs::remove_file(path)
    } else {
        Ok(())
    }
}

fn failed(error: impl std::fmt::Display) -> ExportError {
    ExportError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_core::node::{BlobHash, Node};
    use koru_core::schema::{Parent, WorkspaceDoc};
    use koru_core::{DocumentId, WorkspaceId};
    use koru_export::NoProgress;
    use std::io::Read;

    struct Blobs;
    impl BlobSource for Blobs {
        fn get(&self, _: &BlobHash) -> Result<Vec<u8>, ExportError> {
            Ok(vec![1, 2, 3, 4])
        }
    }

    fn document() -> ExportDocument {
        let mut doc = WorkspaceDoc::create(WorkspaceId::new(), "bundle").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Craft note").unwrap())
            .unwrap();
        doc.insert(Parent::Node(root), Node::paragraph("Hello").unwrap())
            .unwrap();
        let image = Node::image(
            BlobHash(format!("b3:{}", "a".repeat(64))),
            "image/png",
            None,
            None,
        )
        .unwrap();
        doc.insert(Parent::Node(root), image).unwrap();
        ExportDocument {
            id: DocumentId::new(),
            title: "Craft note".into(),
            doc,
        }
    }

    #[test]
    fn package_contains_v2_metadata_markdown_and_assets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Craft note.textbundle");
        TextBundleExporter
            .export(&[document()], &path, &Blobs, &NoProgress)
            .unwrap();

        let info: serde_json::Value =
            serde_json::from_slice(&fs::read(path.join("info.json")).unwrap()).unwrap();
        assert_eq!(info["version"], 2);
        assert_eq!(info["type"], "net.daringfireball.markdown");
        assert_eq!(info["creatorIdentifier"], "do.koru.desktop");
        let markdown = fs::read_to_string(path.join("text.markdown")).unwrap();
        assert!(markdown.contains("# Craft note"));
        assert!(markdown.contains("](assets/"), "{markdown}");
        let asset = fs::read_dir(path.join("assets"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read(asset).unwrap(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn textpack_is_the_same_bundle_in_a_zip_container() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Craft note.textpack");
        TextBundleExporter
            .export(&[document()], &path, &Blobs, &NoProgress)
            .unwrap();

        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        assert!(archive.by_name("info.json").is_ok());
        let mut markdown = String::new();
        archive
            .by_name("text.markdown")
            .unwrap()
            .read_to_string(&mut markdown)
            .unwrap();
        assert!(markdown.contains("](assets/"), "{markdown}");
        assert!(
            archive
                .file_names()
                .any(|name| name.is_ok_and(|name| name.starts_with("assets/")))
        );
    }

    #[test]
    fn zip_extension_writes_the_compressed_textpack_container() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Craft note.zip");
        TextBundleExporter
            .export(&[document()], &path, &Blobs, &NoProgress)
            .unwrap();

        assert!(path.is_file());
        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        assert!(archive.by_name("info.json").is_ok());
        assert!(archive.by_name("text.markdown").is_ok());
    }
}
