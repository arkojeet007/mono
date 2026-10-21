// SPDX-License-Identifier: MPL-2.0

//! TextBundle v2 package import, including compressed TextPack files.
//!
//! A bundle is materialized into a private temporary Markdown tree, then the
//! ordinary Markdown importer performs all semantic conversion. This keeps one
//! parser for files, clipboard content, TextBundle, and future Markdown-based
//! service exports.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use koru_import::{
    ImportContext, ImportError, ImportOptions, ImportPreview, ImportProduct, ImportProgress,
    ImportSource, ImportStage, ImportWarning, Importer, ImporterDescriptor, PreparedImport,
    SourceRequest,
};

pub static DESCRIPTOR: ImporterDescriptor = ImporterDescriptor {
    id: "textbundle",
    name: "TextBundle",
    source: SourceRequest::Files {
        extensions: &["textbundle", "textpack", "zip"],
    },
    menu_name: Some("TextBundle"),
    picker_title: "Import TextBundle packages",
    // The fall-back for a dropped `.zip` no other importer claims: TextPack's
    // conventional alternate extension.
    claims_zip: None,
    roots_import: false,
    dedups_known_files: false,
};

pub struct TextBundleImporter;

impl Importer for TextBundleImporter {
    fn descriptor(&self) -> &'static ImporterDescriptor {
        &DESCRIPTOR
    }

    fn inspect(
        &self,
        source: ImportSource,
        progress: &dyn ImportProgress,
    ) -> Result<Box<dyn PreparedImport>, ImportError> {
        let ImportSource::Files(paths) = source else {
            return Err(ImportError::new(
                "TextBundle import needs one or more packages",
            ));
        };
        if paths.is_empty() {
            return Err(ImportError::new("no TextBundle packages were selected"));
        }

        let staging = tempfile::tempdir()
            .map_err(|error| ImportError::new(format!("could not stage the import: {error}")))?;
        let total = paths.len();
        let mut markdown_paths = Vec::new();
        let mut sources = Vec::new();
        let mut warnings = Vec::new();
        for (index, original) in paths.into_iter().enumerate() {
            if progress.cancelled() {
                return Err(ImportError::cancelled());
            }
            progress.update(ImportStage::Inspecting, index, Some(total));
            let target = staging.path().join(format!("bundle-{index}"));
            match materialize(&original, &target, &mut warnings) {
                Ok(markdown) => {
                    markdown_paths.push(markdown.clone());
                    sources.push((markdown, original));
                }
                Err(error) => warnings.push(ImportWarning {
                    source: original,
                    code: None,
                    arguments: Vec::new(),
                    message: error.to_string(),
                }),
            }
        }
        if markdown_paths.is_empty() {
            let details = warnings
                .first()
                .map(|warning| warning.message.as_str())
                .unwrap_or("none of the selected TextBundles could be read");
            return Err(ImportError::new(details));
        }

        let inner = koru_import_markdown::MarkdownImporter
            .inspect(ImportSource::Files(markdown_paths), progress)?;
        let mut preview = inner.preview().clone();
        preview.warnings.extend(
            warnings
                .iter()
                .map(|warning| format!("{}: {}", warning.source.display(), warning.message)),
        );
        progress.update(ImportStage::Inspecting, total, Some(total));
        Ok(Box::new(PreparedTextBundle {
            inner,
            preview,
            warnings,
            sources,
            _staging: staging,
        }))
    }
}

struct PreparedTextBundle {
    inner: Box<dyn PreparedImport>,
    preview: ImportPreview,
    warnings: Vec<ImportWarning>,
    /// Materialized Markdown path to the package the user selected.
    sources: Vec<(PathBuf, PathBuf)>,
    /// Keeps materialized Markdown and assets alive through `build`.
    _staging: tempfile::TempDir,
}

impl PreparedImport for PreparedTextBundle {
    fn preview(&self) -> &ImportPreview {
        &self.preview
    }

    fn build(
        self: Box<Self>,
        options: ImportOptions,
        context: &ImportContext<'_>,
        progress: &dyn ImportProgress,
    ) -> Result<ImportProduct, ImportError> {
        let PreparedTextBundle {
            inner,
            mut warnings,
            sources,
            _staging,
            ..
        } = *self;
        let product = inner.build(options, context, progress)?;
        let ImportProduct::Documents(mut content) = product else {
            return Err(ImportError::new(
                "the Markdown adapter produced a workspace instead of documents",
            ));
        };
        for document in &mut content.documents {
            if let Some((_, original)) = sources
                .iter()
                .find(|(materialized, _)| materialized == &document.source)
            {
                document.source = original.clone();
            }
        }
        for warning in &mut content.warnings {
            if let Some((_, original)) = sources
                .iter()
                .find(|(materialized, _)| materialized == &warning.source)
            {
                warning.source = original.clone();
            }
        }
        warnings.append(&mut content.warnings);
        content.warnings = warnings;
        Ok(ImportProduct::Documents(content))
    }
}

const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 64 * 1024 * 1024;

fn materialize(
    source: &Path,
    target: &Path,
    warnings: &mut Vec<ImportWarning>,
) -> Result<PathBuf, ImportError> {
    if source.is_dir() {
        materialize_package(source, target, warnings)
    } else {
        materialize_archive(source, target, warnings)
    }
}

fn materialize_package(
    source: &Path,
    target: &Path,
    warnings: &mut Vec<ImportWarning>,
) -> Result<PathBuf, ImportError> {
    let entries: Vec<_> = fs::read_dir(source)
        .map_err(failed)?
        .collect::<Result<_, _>>()
        .map_err(failed)?;
    let text = entries
        .iter()
        .find(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file()) && is_text_name(&entry.file_name())
        })
        .map(|entry| entry.path())
        .ok_or_else(|| ImportError::new("the package has no text.* document"))?;
    if text.metadata().map_err(failed)?.len() > MAX_TEXT_BYTES {
        return Err(ImportError::new("the TextBundle document is too large"));
    }

    let info = entries
        .iter()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("info.json")
        })
        .map(|entry| entry.path());
    validate_info(info.as_deref(), source, warnings)?;

    fs::create_dir_all(target).map_err(failed)?;
    let markdown = target.join(format!("{}.md", safe_stem(source)));
    fs::copy(&text, &markdown).map_err(failed)?;
    if let Some(assets) = entries
        .iter()
        .find(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("assets")
        })
        .map(|entry| entry.path())
    {
        copy_tree(&assets, &target.join("assets"))?;
    }
    Ok(markdown)
}

fn materialize_archive(
    source: &Path,
    target: &Path,
    warnings: &mut Vec<ImportWarning>,
) -> Result<PathBuf, ImportError> {
    let file = File::open(source).map_err(failed)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        ImportError::new(format!(
            "the selection is neither a TextBundle package nor a TextPack archive: {error}"
        ))
    })?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(ImportError::new("the TextPack contains too many entries"));
    }

    let mut names = Vec::with_capacity(archive.len());
    let mut unique = HashSet::new();
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(failed)?;
        let path = entry.enclosed_name().ok_or_else(|| {
            ImportError::new(format!(
                "unsafe archive path: {}",
                String::from_utf8_lossy(entry.name_raw())
            ))
        })?;
        if !unique.insert(path.clone()) {
            return Err(ImportError::new("the TextPack contains duplicate paths"));
        }
        if is_link(entry.unix_mode()) || (!entry.is_file() && !entry.is_dir()) {
            return Err(ImportError::new(
                "the TextPack contains a link or unsupported entry",
            ));
        }
        expanded = expanded
            .checked_add(entry.size())
            .ok_or_else(|| ImportError::new("the TextPack size overflowed"))?;
        if expanded > MAX_ARCHIVE_BYTES {
            return Err(ImportError::new(
                "the TextPack expands beyond the 512 MB safety limit",
            ));
        }
        names.push((index, path, entry.is_file(), entry.size()));
    }

    let text = names
        .iter()
        .filter(|(_, path, is_file, _)| *is_file && path.file_name().is_some_and(is_text_name))
        .min_by_key(|(_, path, _, _)| {
            let base = path.parent().unwrap_or_else(|| Path::new(""));
            let has_info = names.iter().any(|(_, candidate, file, _)| {
                *file
                    && candidate.parent() == Some(base)
                    && candidate.file_name().is_some_and(|name| {
                        name.to_string_lossy().eq_ignore_ascii_case("info.json")
                    })
            });
            (!has_info, path.components().count())
        })
        .cloned()
        .ok_or_else(|| ImportError::new("the TextPack has no text.* document"))?;
    if text.3 > MAX_TEXT_BYTES {
        return Err(ImportError::new("the TextBundle document is too large"));
    }
    let base = text
        .1
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();

    if let Some((info_index, _, _, _)) = names.iter().find(|(_, path, is_file, _)| {
        *is_file
            && path.parent() == Some(base.as_path())
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("info.json"))
    }) {
        let bytes = read_entry(&mut archive, *info_index)?;
        validate_info_bytes(Some(&bytes), source, warnings)?;
    } else {
        validate_info_bytes(None, source, warnings)?;
    }

    fs::create_dir_all(target).map_err(failed)?;
    let markdown = target.join(format!("{}.md", safe_stem(source)));
    write_bytes(&markdown, &read_entry(&mut archive, text.0)?)?;
    let asset_root = base.join("assets");
    for (index, path, is_file, _) in names {
        if !is_file || !starts_with_case_insensitive(&path, &asset_root) {
            continue;
        }
        let relative = strip_prefix_case_insensitive(&path, &asset_root)
            .ok_or_else(|| ImportError::new("invalid asset path"))?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let output = target.join("assets").join(relative);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(failed)?;
        }
        write_bytes(&output, &read_entry(&mut archive, index)?)?;
    }
    Ok(markdown)
}

fn read_entry(archive: &mut zip::ZipArchive<File>, index: usize) -> Result<Vec<u8>, ImportError> {
    let mut entry = archive.by_index(index).map_err(failed)?;
    let mut bytes = Vec::with_capacity(entry.size().min(usize::MAX as u64) as usize);
    entry.read_to_end(&mut bytes).map_err(failed)?;
    Ok(bytes)
}

fn validate_info(
    path: Option<&Path>,
    source: &Path,
    warnings: &mut Vec<ImportWarning>,
) -> Result<(), ImportError> {
    let bytes = path.map(fs::read).transpose().map_err(failed)?;
    validate_info_bytes(bytes.as_deref(), source, warnings)
}

fn validate_info_bytes(
    bytes: Option<&[u8]>,
    source: &Path,
    warnings: &mut Vec<ImportWarning>,
) -> Result<(), ImportError> {
    let Some(bytes) = bytes else {
        warnings.push(ImportWarning {
            source: source.to_path_buf(),
            code: None,
            arguments: Vec::new(),
            message: "The package has no info.json; Markdown was imported for compatibility."
                .into(),
        });
        return Ok(());
    };
    let info: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| ImportError::new(format!("info.json is not valid JSON: {error}")))?;
    if let Some(version) = info.get("version") {
        let version = version
            .as_u64()
            .ok_or_else(|| ImportError::new("info.json version must be an integer"))?;
        if !(1..=2).contains(&version) {
            return Err(ImportError::new(format!(
                "TextBundle version {version} is not supported"
            )));
        }
    } else {
        warnings.push(ImportWarning {
            source: source.to_path_buf(),
            code: None,
            arguments: Vec::new(),
            message: "info.json has no version; Markdown was imported for compatibility.".into(),
        });
    }
    if let Some(kind) = info.get("type").and_then(|kind| kind.as_str())
        && kind != "net.daringfireball.markdown"
    {
        return Err(ImportError::new(format!(
            "the TextBundle contains {kind}, not Markdown"
        )));
    }
    Ok(())
}

fn is_text_name(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy()
        .to_ascii_lowercase()
        .starts_with("text.")
}

fn safe_stem(source: &Path) -> String {
    let raw = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled");
    let safe: String = raw
        .chars()
        .map(|character| {
            if matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            ) {
                '-'
            } else {
                character
            }
        })
        .collect();
    let safe = safe.trim().trim_matches('.').trim();
    if safe.is_empty() {
        "Untitled".into()
    } else {
        safe.into()
    }
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), ImportError> {
    fs::create_dir_all(destination).map_err(failed)?;
    for entry in fs::read_dir(source).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let kind = entry.file_type().map_err(failed)?;
        if kind.is_symlink() {
            return Err(ImportError::new(
                "the TextBundle assets directory contains a symbolic link",
            ));
        }
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target).map_err(failed)?;
        }
    }
    Ok(())
}

fn starts_with_case_insensitive(path: &Path, prefix: &Path) -> bool {
    let path: Vec<_> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    let prefix: Vec<_> = prefix
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    path.starts_with(&prefix)
}

fn strip_prefix_case_insensitive(path: &Path, prefix: &Path) -> Option<PathBuf> {
    let path_components: Vec<_> = path.components().collect();
    let prefix_components: Vec<_> = prefix.components().collect();
    if path_components.len() < prefix_components.len()
        || !path_components
            .iter()
            .zip(&prefix_components)
            .all(|(left, right)| {
                left.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
            })
    {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in &path_components[prefix_components.len()..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

fn is_link(mode: Option<u32>) -> bool {
    mode.is_some_and(|mode| mode & 0o170000 == 0o120000)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), ImportError> {
    let mut file = File::create(path).map_err(failed)?;
    file.write_all(bytes).map_err(failed)
}

fn failed(error: impl std::fmt::Display) -> ImportError {
    ImportError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_import::{BlobSink, IdentityMode, StoredBlob};
    use std::sync::Mutex;
    use zip::write::SimpleFileOptions;

    #[derive(Default)]
    struct Sink(Mutex<Vec<Vec<u8>>>);
    impl BlobSink for Sink {
        fn put(
            &self,
            bytes: Vec<u8>,
            mime: &str,
            _: Option<&str>,
        ) -> Result<StoredBlob, ImportError> {
            self.0.lock().unwrap().push(bytes);
            Ok(StoredBlob {
                hash: format!("b3:{}", "0".repeat(64)),
                mime: mime.into(),
            })
        }
    }

    #[test]
    fn package_uses_bundle_name_as_fallback_title_and_imports_assets() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Craft note.textbundle");
        fs::create_dir_all(bundle.join("assets")).unwrap();
        fs::write(
            bundle.join("info.json"),
            r#"{"version":2,"type":"net.daringfireball.markdown"}"#,
        )
        .unwrap();
        fs::write(
            bundle.join("text.markdown"),
            "Hello\n\n![](assets/photo.png)",
        )
        .unwrap();
        fs::write(bundle.join("assets/photo.png"), [1, 2, 3]).unwrap();

        let prepared = TextBundleImporter
            .inspect(
                ImportSource::Files(vec![bundle.clone()]),
                &koru_import::NoProgress,
            )
            .unwrap();
        assert_eq!(prepared.preview().documents[0].title, "Craft note");
        let sink = Sink::default();
        let product = prepared
            .build(
                ImportOptions {
                    identity: IdentityMode::Copy,
                    accept_warnings: true,
                },
                &ImportContext {
                    target_profile: None,
                    target_workspace: Some(koru_store_core::ids::WorkspaceId::new()),
                    occupied_titles: &std::collections::BTreeSet::new(),
                    blobs: &sink,
                    tags: &koru_import::NoTagSink,
                },
                &koru_import::NoProgress,
            )
            .unwrap();
        let ImportProduct::Documents(content) = product else {
            panic!("TextBundle produced a workspace")
        };
        assert_eq!(content.documents[0].source, bundle);
        assert_eq!(content.documents[0].title, "Craft note");
        assert_eq!(*sink.0.lock().unwrap(), vec![vec![1, 2, 3]]);
    }

    #[test]
    fn textpack_accepts_a_wrapper_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Bear export.textpack");
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("Note.textbundle/info.json", options)
            .unwrap();
        zip.write_all(br#"{"version":2}"#).unwrap();
        zip.start_file("Note.textbundle/text.md", options).unwrap();
        zip.write_all(b"# Packed\n\nBody").unwrap();
        zip.finish().unwrap();

        let prepared = TextBundleImporter
            .inspect(ImportSource::Files(vec![path]), &koru_import::NoProgress)
            .unwrap();
        assert_eq!(prepared.preview().documents[0].title, "Packed");
    }

    #[test]
    fn zip_extension_is_accepted_as_a_compressed_textpack() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Craft export.zip");
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("info.json", options).unwrap();
        zip.write_all(br#"{"version":2,"type":"net.daringfireball.markdown"}"#)
            .unwrap();
        zip.start_file("text.markdown", options).unwrap();
        zip.write_all(b"# Zipped\n\nBody").unwrap();
        zip.finish().unwrap();

        let prepared = TextBundleImporter
            .inspect(ImportSource::Files(vec![path]), &koru_import::NoProgress)
            .unwrap();
        assert_eq!(prepared.preview().documents[0].title, "Zipped");
    }

    #[test]
    fn a_non_markdown_bundle_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Wrong.textbundle");
        fs::create_dir(&bundle).unwrap();
        fs::write(
            bundle.join("info.json"),
            r#"{"version":2,"type":"com.apple.rtf"}"#,
        )
        .unwrap();
        fs::write(bundle.join("text.rtf"), "not Markdown").unwrap();
        let error = TextBundleImporter
            .inspect(ImportSource::Files(vec![bundle]), &koru_import::NoProgress)
            .err()
            .expect("wrong type is rejected");
        assert!(error.to_string().contains("not Markdown"));
    }
}
