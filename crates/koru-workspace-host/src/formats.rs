// SPDX-License-Identifier: MPL-2.0

//! The one construction site for the format registry.
//!
//! Importers and exporters are each registered exactly once, here. The
//! background workers build their `ImportRegistry`/`ExportRegistry` from these
//! lists, and the UI builds its menus from the descriptor tables derived from
//! the same lists — so a menu row, a file dialog, a drop route and the worker
//! that serves them can never drift apart. Format crates stay shell-only:
//! `apps/koru` reaches this module through `koru_workspace_host::formats` and names
//! descriptors, never importers.

use std::path::Path;

use koru_export::{Exporter, ExporterDescriptor};
use koru_i18n::Key;
use koru_import::{ImportWarningCode, Importer, ImporterDescriptor, SourceRequest};

/// The one registry row for an importer and its shell-owned localization.
pub struct RegisteredImporter {
    pub importer: Box<dyn Importer>,
    pub menu_key: Option<Key>,
    pub picker_key: Option<Key>,
    /// Why this importer cannot run in this build, when it cannot — offered
    /// dimmed with this reason rather than hidden, so a community build says
    /// what it is missing.
    pub unavailable_key: Option<Key>,
}

/// The localized menu and picker labels owned by the shell.
pub const EVERNOTE_KEYS: (Key, Key) = (Key::ImportEvernoteMenu, Key::ImportEvernotePicker);

/// Every content importer the shell can run, in registration order. Order is
/// visible: it is the menu order, and it is the order `.zip` sniffers are
/// consulted in for a dropped file.
pub fn registered_importers(
    credentials: Option<koru_oauth_credentials::CredentialContext>,
) -> Vec<RegisteredImporter> {
    let legacy = |importer: Box<dyn Importer>| RegisteredImporter {
        importer,
        menu_key: None,
        picker_key: None,
        unavailable_key: None,
    };
    vec![
        legacy(Box::new(koru_import_markdown::MarkdownImporter)),
        RegisteredImporter {
            importer: Box::new(koru_import_evernote::EvernoteImporter),
            menu_key: Some(EVERNOTE_KEYS.0),
            picker_key: Some(EVERNOTE_KEYS.1),
            unavailable_key: None,
        },
        legacy(Box::new(koru_import_textbundle::TextBundleImporter)),
        legacy(Box::new(koru_import_onenote::OneNoteImporter)),
        legacy(Box::new(koru_import_files::FileImporter)),
        // The importer opens the Google consent screen through the one door
        // this workspace has for handing a URI to the desktop. The opener is
        // injected rather than linked: `koru_editor::browser` is the other
        // opener and is wrong here twice over — its allow-list exists for
        // *remote* input, where this URL is built in-process from a
        // compile-time endpoint, and this crate does not depend on
        // `koru-editor`.
        //
        // **Its OAuth registration is build configuration** (C6): an official
        // build bakes Koru's, a community build may supply its own, and a build
        // with none offers the row dimmed with the reason.
        RegisteredImporter {
            importer: Box::new(koru_import_google_docs::GoogleDocsImporter::new(
                koru_window::open_uri,
                credentials,
            )),
            menu_key: None,
            picker_key: None,
            unavailable_key: (!koru_import_google_docs::is_configured())
                .then_some(Key::NotSetUpInThisBuild),
        },
        legacy(Box::new(koru_import_notion::NotionImporter)),
    ]
}

pub fn importers(
    credentials: Option<koru_oauth_credentials::CredentialContext>,
) -> Vec<Box<dyn Importer>> {
    registered_importers(credentials)
        .into_iter()
        .map(|registered| registered.importer)
        .collect()
}

/// Every document exporter the shell can run, in menu order.
pub fn exporters() -> Vec<Box<dyn Exporter>> {
    vec![
        Box::new(koru_export_markdown::MarkdownExporter),
        Box::new(koru_export_textbundle::TextBundleExporter),
    ]
}

/// The descriptor behind a stable format id, import direction.
pub fn import_descriptor(id: &str) -> Option<&'static ImporterDescriptor> {
    registered_importers(None)
        .into_iter()
        .map(|registered| registered.importer.descriptor())
        .find(|descriptor| descriptor.id == id)
}

pub fn import_menu_name(id: &str, lang: koru_i18n::Lang) -> Option<&'static str> {
    registered_importers(None)
        .into_iter()
        .find_map(|registered| {
            let descriptor = registered.importer.descriptor();
            (descriptor.id == id).then(|| {
                registered
                    .menu_key
                    .map(|key| koru_i18n::t(lang, key))
                    .or(descriptor.menu_name)
                    .unwrap_or(descriptor.name)
            })
        })
}

pub fn import_picker_title(id: &str, lang: koru_i18n::Lang) -> Option<&'static str> {
    registered_importers(None)
        .into_iter()
        .find_map(|registered| {
            let descriptor = registered.importer.descriptor();
            (descriptor.id == id).then(|| {
                registered
                    .picker_key
                    .map(|key| koru_i18n::t(lang, key))
                    .unwrap_or(descriptor.picker_title)
            })
        })
}

/// Why an importer cannot run in this build, localized, or `None` when it
/// can. A menu dims the row and shows this beside it.
pub fn import_unavailable_reason(id: &str, lang: koru_i18n::Lang) -> Option<&'static str> {
    registered_importers(None)
        .into_iter()
        .find(|registered| registered.importer.descriptor().id == id)
        .and_then(|registered| registered.unavailable_key)
        .map(|key| koru_i18n::t(lang, key))
}

pub fn import_warning_key(code: ImportWarningCode) -> Option<Key> {
    Some(match code {
        ImportWarningCode::EnexEnvelopeTruncated => Key::ImportWarningEnexEnvelopeTruncated,
        ImportWarningCode::EnexSecurityRejected => Key::ImportWarningEnexSecurityRejected,
        ImportWarningCode::InvalidSourceTimestamp => Key::ImportWarningInvalidSourceTimestamp,
        ImportWarningCode::EvernoteLinkAmbiguous => Key::ImportWarningEvernoteLinkAmbiguous,
        ImportWarningCode::EvernoteTaskPayloadUnsupported => {
            Key::ImportWarningEvernoteTaskUnsupported
        }
        ImportWarningCode::EvernoteReminderNotMapped => Key::ImportWarningEvernoteReminderNotMapped,
        ImportWarningCode::InlineCheckboxSimplified => Key::ImportWarningInlineCheckboxSimplified,
        ImportWarningCode::EvernoteMissingResource => Key::ImportWarningEvernoteMissingResource,
        ImportWarningCode::EvernoteInvalidResource => Key::ImportWarningEvernoteInvalidResource,
        ImportWarningCode::EvernoteMimeMismatch => Key::ImportWarningEvernoteMimeMismatch,
        ImportWarningCode::EvernoteUnknownElement => Key::ImportWarningEvernoteUnknownElement,
        ImportWarningCode::EvernoteMergedTableCells => Key::ImportWarningEvernoteMergedTableCells,
    })
}

/// The descriptor behind a stable format id, export direction.
pub fn export_descriptor(id: &str) -> Option<&'static ExporterDescriptor> {
    exporters()
        .into_iter()
        .map(|exporter| exporter.descriptor())
        .find(|descriptor| descriptor.id == id)
}

/// What an import menu lists, in order. Importers without a `menu_name`
/// (`files`, the legacy workspace restore) are service importers and stay
/// out of every menu.
pub fn import_menu_formats() -> Vec<&'static ImporterDescriptor> {
    registered_importers(None)
        .into_iter()
        .map(|registered| registered.importer.descriptor())
        .filter(|descriptor| descriptor.menu_name.is_some())
        .collect()
}

/// What an export menu lists, in order.
pub fn export_menu_formats() -> Vec<&'static ExporterDescriptor> {
    exporters()
        .into_iter()
        .map(|exporter| exporter.descriptor())
        .collect()
}

/// Import format selected by an externally dropped path, read from the
/// registered descriptors. A `.zip` is offered to each claimant's
/// `claims_zip` sniffer in registration order; a zip nobody claims falls to
/// the importer that advertises `zip` without a sniffer (TextBundle).
pub fn dropped_content_format(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let descriptors: Vec<&'static ImporterDescriptor> = registered_importers(None)
        .into_iter()
        .map(|registered| registered.importer.descriptor())
        .collect();
    let mut zip_fallback = None;
    for descriptor in descriptors {
        let SourceRequest::Files { extensions } = descriptor.source else {
            continue;
        };
        if !extensions.contains(&extension.as_str()) {
            continue;
        }
        if extension == "zip" {
            match descriptor.claims_zip {
                Some(claims) if claims(&zip_entry_names(path)) => {
                    return Some(descriptor.id);
                }
                // A sniffer that declines keeps looking; one zip can name
                // several formats by extension.
                Some(_) => continue,
                None => {
                    zip_fallback.get_or_insert(descriptor.id);
                }
            }
        } else {
            return Some(descriptor.id);
        }
    }
    zip_fallback
}

/// Entry names of a dropped archive, for the `claims_zip` sniffers. An
/// unreadable zip is no names, which is a decline from every sniffer.
fn zip_entry_names(path: &Path) -> Vec<String> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Vec::new();
    };
    (0..archive.len())
        .filter_map(|index| {
            let entry = archive.by_index(index).ok()?;
            let name = entry.name().ok()?.into_owned();
            Some(name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a zip with the given entry names and return its path; the
    /// `tempfile::TempDir` must outlive the assertion that reads it.
    fn zip_with(entries: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drop.zip");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for entry in entries {
            writer
                .start_file::<_, ()>(entry, zip::write::FileOptions::default())
                .unwrap();
        }
        writer.finish().unwrap();
        (dir, path)
    }

    #[test]
    fn the_menu_lists_only_menu_formats_in_registration_order() {
        let ids: Vec<&str> = import_menu_formats()
            .into_iter()
            .map(|descriptor| descriptor.id)
            .collect();
        assert_eq!(
            ids,
            [
                "markdown",
                "evernote",
                "textbundle",
                "onenote",
                "google-docs",
                "notion"
            ]
        );
    }

    #[test]
    fn descriptors_are_found_by_stable_id() {
        assert_eq!(
            import_descriptor("onenote").unwrap().name,
            "Microsoft OneNote"
        );
        assert!(import_descriptor("koru-legacy-workspace").is_none());
        assert_eq!(export_descriptor("markdown").unwrap().extensions, ["md"]);
        assert!(export_descriptor("pdf").is_none());
    }

    #[test]
    fn localization_keys_cannot_drift_from_importer_registration() {
        for registered in registered_importers(None) {
            assert_eq!(
                registered.menu_key.is_some(),
                registered.picker_key.is_some(),
                "{} has exactly one localization key",
                registered.importer.descriptor().id
            );
        }
        let markdown = registered_importers(None)
            .into_iter()
            .find(|registered| registered.importer.descriptor().id == "markdown")
            .unwrap();
        assert!(markdown.menu_key.is_none() && markdown.picker_key.is_none());

        let evernote = registered_importers(None)
            .into_iter()
            .filter(|registered| registered.importer.descriptor().id == "evernote")
            .collect::<Vec<_>>();
        assert_eq!(evernote.len(), 1);
        assert_eq!(evernote[0].menu_key, Some(Key::ImportEvernoteMenu));
        assert_eq!(evernote[0].picker_key, Some(Key::ImportEvernotePicker));
    }

    #[test]
    fn only_an_unconfigured_google_docs_importer_is_unavailable() {
        for registered in registered_importers(None) {
            let id = registered.importer.descriptor().id;
            if id == "google-docs" {
                assert_eq!(
                    registered.unavailable_key.is_some(),
                    !koru_import_google_docs::is_configured(),
                    "the Google Docs row is dimmed exactly when this build has no registration"
                );
                if let Some(key) = registered.unavailable_key {
                    assert_eq!(key, Key::NotSetUpInThisBuild);
                    for lang in koru_i18n::Lang::ALL {
                        assert_eq!(
                            import_unavailable_reason(id, lang),
                            Some(koru_i18n::t(lang, key))
                        );
                    }
                }
            } else {
                assert!(
                    registered.unavailable_key.is_none(),
                    "{id} is always available"
                );
                assert_eq!(import_unavailable_reason(id, koru_i18n::Lang::En), None);
            }
        }
    }

    #[test]
    fn every_warning_code_has_a_localization_key() {
        use ImportWarningCode::*;
        for code in [
            EnexEnvelopeTruncated,
            EnexSecurityRejected,
            InvalidSourceTimestamp,
            EvernoteLinkAmbiguous,
            EvernoteTaskPayloadUnsupported,
            EvernoteReminderNotMapped,
            InlineCheckboxSimplified,
            EvernoteMissingResource,
            EvernoteInvalidResource,
            EvernoteMimeMismatch,
            EvernoteUnknownElement,
            EvernoteMergedTableCells,
        ] {
            assert!(import_warning_key(code).is_some(), "{}", code.as_str());
        }
    }

    #[test]
    fn export_menus_mirror_the_registered_exporters() {
        let ids: Vec<&str> = export_menu_formats()
            .into_iter()
            .map(|descriptor| descriptor.id)
            .collect();
        assert_eq!(ids, ["markdown", "textbundle"]);
    }

    #[test]
    fn dropped_files_route_by_registered_extension() {
        assert_eq!(
            dropped_content_format(Path::new("note.md")),
            Some("markdown")
        );
        assert_eq!(
            dropped_content_format(Path::new("note.markdown")),
            Some("markdown")
        );
        assert_eq!(
            dropped_content_format(Path::new("book.onepkg")),
            Some("onenote")
        );
        assert_eq!(
            dropped_content_format(Path::new("craft.textbundle")),
            Some("textbundle")
        );
        assert_eq!(
            dropped_content_format(Path::new("Archive.ENEX")),
            Some("evernote")
        );
        assert_eq!(dropped_content_format(Path::new("notes.txt")), None);
    }

    #[test]
    fn a_zip_nobody_claims_falls_to_textbundle() {
        let (_dir, path) = zip_with(&["text.md", "assets/logo.png"]);
        assert_eq!(dropped_content_format(&path), Some("textbundle"));
        // An unreadable zip is declined by every sniffer and still falls back.
        assert_eq!(
            dropped_content_format(Path::new("missing.zip")),
            Some("textbundle")
        );
    }

    #[test]
    fn a_zip_with_onenote_members_routes_to_onenote() {
        let (_dir, path) = zip_with(&["Notebook/Section.one"]);
        assert_eq!(dropped_content_format(&path), Some("onenote"));
    }

    #[test]
    fn a_zip_with_notion_id_filenames_routes_to_notion() {
        let (_dir, path) = zip_with(&["Workspace 11111111111111111111111111111111.md"]);
        assert_eq!(dropped_content_format(&path), Some("notion"));
    }
}
