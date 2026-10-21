// SPDX-License-Identifier: MPL-2.0

//! **Nothing here reaches the network, Cloudflare, or GitHub.**
//!
//! The two things this tool does that are expensive to get wrong are *the JSON
//! the store will parse* and *the licence inside the zip*, and both are
//! checkable from a fixture: the catalog is round-tripped through
//! `koru_voice_models`' real parser, and the archive is built from a `.tar.bz2`
//! this file writes rather than one it downloads. The origin's answers are
//! parameters — see [`super::archive_job`] — so a re-run's skip decision is a
//! pure function with a literal on the other side of it.

use super::*;
use koru_voice_models::{Catalog, ModelTier};

/// A row whose upstream artifact is the fixture built below, pinned to it.
fn fixture_row(digest: &'static str) -> ModelArtifact {
    ModelArtifact {
        id: "fixture-model",
        version: "int8-1",
        engine: "sherpa",
        tier: ModelTier::Compact,
        languages: &["en", "ja"],
        shape: table::Shape::TarBz2,
        url: "https://example.invalid/fixture.tar.bz2",
        file_name: "fixture.tar.bz2",
        upstream_size: 0,
        upstream_sha256: digest,
        repacked_sha256: "",
        working_set_bytes: 1024 * 1024 * 1024,
        licence: "CC-BY-4.0",
        licence_file: "CC-BY-4.0.txt",
        attribution: "Somebody Else, Fixture Model (CC-BY-4.0)",
        origin_notice: &["Fixture Model", "Copyright (c) 2026 Somebody Else."],
    }
}

/// A `.tar.bz2` shaped like the real ones: one top-level directory, model files
/// beneath it. Built rather than checked in, for the reason
/// `koru-voice-models`' own fixtures give — a checked-in archive goes stale
/// silently and the repair is the step somebody skips.
fn write_fixture_tar(path: &Path, entries: &[(&str, &[u8])]) {
    use bzip2::write::BzEncoder;
    let file = std::fs::File::create(path).unwrap();
    let encoder = BzEncoder::new(file, bzip2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("fixture-model-dir/{name}"), *bytes)
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

fn options(work: &Path) -> Options {
    Options {
        work: work.to_path_buf(),
        sources: Vec::new(),
        only: None,
        upload: false,
        keep: false,
        base_url: None,
    }
}

#[test]
fn product_rows_are_exactly_the_two_visible_quality_tiers() {
    let rows = table::product_artifacts().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| row.id == "whisper-base"));
    assert!(
        rows.iter()
            .any(|row| row.id == "nemotron-3.5-streaming-0.6b-160ms")
    );
}

#[test]
fn default_work_is_owned_by_the_main_checkout() {
    let root = Path::new("checkout");
    assert_eq!(
        shared_work_from_common(root, ".git").unwrap(),
        Path::new("checkout").join("dist/voice-models")
    );
}

// ------------------------------------------------------------- the table

/// **Every row is a descriptor the store would accept.** Ids, versions,
/// engines, licences and language tags are all newtypes with their own rules,
/// and a row that breaks one of them would otherwise be discovered by a person
/// half a gigabyte into a publish.
#[test]
fn every_row_builds_a_valid_catalog_entry() {
    let digest = "a".repeat(64);
    for artifact in table::ARTIFACTS {
        let entry = catalog::entry(artifact, 1234, &digest)
            .unwrap_or_else(|error| panic!("{}: {error}", artifact.id));
        assert_eq!(entry.descriptor.id.as_str(), artifact.id);
        assert_eq!(entry.descriptor.version.as_str(), artifact.version);
        assert_eq!(entry.descriptor.size, 1234);
        // The canonical form is what a signature will one day cover, so it has
        // to render today even though nothing signs it.
        entry.descriptor.canonical_bytes().unwrap();
    }
}

/// Every row's licence text is compiled in. `include_str!` already makes a
/// missing *file* a build error; this makes a row naming a text nobody wired
/// up a test failure rather than a package published without its licence.
#[test]
fn every_row_has_its_licence_text_compiled_in() {
    for artifact in table::ARTIFACTS {
        let text = repack::licence_text(artifact.licence_file)
            .unwrap_or_else(|error| panic!("{}: {error}", artifact.id));
        assert!(
            text.len() > 500,
            "{} carries a suspiciously short licence text",
            artifact.id
        );
    }
}

/// A content-addressed zip must not inherit the checkout's line-ending policy.
/// Windows materializes generic text files as CRLF in this repository, while
/// the digest in the model table was measured from LF bytes.
#[test]
fn embedded_licence_text_has_one_platform_independent_line_ending() {
    assert_eq!(
        repack::normalize_line_endings("first\r\nsecond\rthird\n"),
        "first\nsecond\nthird\n"
    );
    for artifact in table::ARTIFACTS {
        let text = repack::licence_text(artifact.licence_file).unwrap();
        assert!(
            !text.contains('\r'),
            "{} embeds a carriage return in its licence",
            artifact.id
        );
    }
}

/// **At least one model is `Compact`.** `ModelTier::default()` is `Compact` and
/// `Catalog::choose` matches the tier *exactly* — deliberately, so a request
/// for accuracy never quietly settles. A catalog with no compact entry
/// therefore offers a new person nothing at all, and no error would say so.
#[test]
fn the_catalog_offers_something_at_the_default_tier() {
    assert!(
        table::ARTIFACTS
            .iter()
            .any(|artifact| artifact.tier == ModelTier::default()),
        "no row is Compact, so the product's default tier resolves to nothing"
    );
}

/// The two Nemotron rows are two artifacts, not one model and a flag — Q-V52's
/// finding, pinned where somebody would be tempted to merge them.
#[test]
fn the_two_nemotron_chunk_sizes_are_two_rows() {
    let nemotron: Vec<_> = table::ARTIFACTS
        .iter()
        .filter(|artifact| artifact.id.starts_with("nemotron-"))
        .collect();
    assert_eq!(nemotron.len(), 2);
    assert_ne!(nemotron[0].id, nemotron[1].id);
    assert_ne!(nemotron[0].url, nemotron[1].url);
}

// ------------------------------------------------------------ the archive

/// **The licence and the notices are inside the zip.** OpenMDW-1.1 permits
/// redistribution *provided* the agreement and the origin notices are retained
/// in the distribution, and CC-BY-4.0 and MIT each want their own notice, so
/// this is a legal condition rather than a nicety.
#[test]
fn the_repackaged_zip_carries_the_licence_and_the_notices() {
    let dir = tempfile::tempdir().unwrap();
    let tar = dir.path().join("fixture.tar.bz2");
    write_fixture_tar(
        &tar,
        &[("encoder.int8.onnx", b"weights"), ("tokens.txt", b"a b c")],
    );
    let artifact = fixture_row("");
    let zip = dir.path().join("out.zip");
    let packaged = repack::repack(&artifact, &tar, &zip, "").unwrap();
    assert_eq!(packaged.size, std::fs::metadata(&zip).unwrap().len());

    let names = zip_names(&zip);
    assert!(names.contains(&repack::LICENCE_FILE.to_string()));
    assert!(names.contains(&repack::NOTICE_FILE.to_string()));

    let licence = zip_entry(&zip, repack::LICENCE_FILE);
    assert_eq!(licence, repack::licence_text("CC-BY-4.0.txt").unwrap());

    let notice = zip_entry(&zip, repack::NOTICE_FILE);
    assert!(notice.contains("Copyright (c) 2026 Somebody Else."));
    assert!(notice.contains("CC-BY-4.0"));
    assert!(notice.contains("https://example.invalid/fixture.tar.bz2"));
    // The repackaging is stated as a change, which is what CC-BY-4.0 asks for
    // and what makes the provenance chain readable from the artifact itself.
    assert!(notice.contains("repackaged as a"));
}

/// **The tar's top-level directory is stripped.** Keeping it would install a
/// model whose graphs are one level below where the adapter looks, and the only
/// symptom would be a directory that visibly contains `encoder.int8.onnx` being
/// reported as having neither.
#[test]
fn the_tars_top_level_directory_does_not_survive() {
    let dir = tempfile::tempdir().unwrap();
    let tar = dir.path().join("fixture.tar.bz2");
    write_fixture_tar(&tar, &[("encoder.int8.onnx", b"weights")]);
    let zip = dir.path().join("out.zip");
    repack::repack(&fixture_row(""), &tar, &zip, "").unwrap();
    let names = zip_names(&zip);
    assert!(names.contains(&"encoder.int8.onnx".to_string()));
    assert!(
        !names.iter().any(|name| name.contains("fixture-model-dir")),
        "the wrapper directory survived: {names:?}"
    );
}

/// **The same input always produces the same zip**, which is what makes
/// `repacked_sha256` a shortcut rather than a trap: the digest is the published
/// key, so a zip that varied between runs would re-upload half a gigabyte every
/// time and pin nothing.
#[test]
fn repacking_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let tar = dir.path().join("fixture.tar.bz2");
    write_fixture_tar(&tar, &[("a.onnx", b"one"), ("b/c.txt", b"two")]);
    let artifact = fixture_row("");
    let first = repack::repack(&artifact, &tar, &dir.path().join("one.zip"), "").unwrap();
    let second = repack::repack(&artifact, &tar, &dir.path().join("two.zip"), "").unwrap();
    assert_eq!(first.digest, second.digest);
    assert_eq!(first.size, second.size);
}

// ------------------------------------------------------- digest discipline

/// A local catalog must never turn an on-disk zip that disagrees with the
/// table into a newly trusted content-addressed object. `fetch` is the one path
/// that may rebuild it from the separately pinned upstream bytes.
#[test]
fn a_repacked_zip_that_does_not_match_its_pin_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let expected = Box::leak(digest_of(b"the expected zip").into_boxed_str());
    let artifact = ModelArtifact {
        repacked_sha256: expected,
        ..fixture_row("")
    };
    let options = options(dir.path());
    std::fs::write(zip_path(&artifact, &options), b"some other zip").unwrap();

    let error = known_digest(&artifact, &options).unwrap_err();
    assert!(error.contains("not its pinned repacked digest"), "{error}");
    assert!(
        error.contains("voice-models fetch --only fixture-model"),
        "{error}"
    );
}

/// **A corrupted archive is refused, and nothing is overwritten.**
///
/// The bytes are already on disk here, so no network is involved: the file
/// under the artifact's name hashes to something other than the pin, and the
/// tool declines rather than repackaging it or quietly replacing it. Declining
/// rather than replacing is the same rule `fetch-voice-runtime` follows —
/// somebody put that file there and this is not the thing that should decide
/// they were wrong.
#[test]
fn a_download_that_does_not_match_its_pin_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let good = b"the bytes we pinned";
    let artifact = ModelArtifact {
        upstream_size: good.len() as u64,
        ..fixture_row(Box::leak(digest_of(good).into_boxed_str()))
    };
    let target = dir.path().join(artifact.file_name);
    std::fs::write(&target, b"something else entirely").unwrap();

    let error = ensure_downloaded(&artifact, &options(dir.path())).unwrap_err();
    assert!(error.contains("not the pinned"), "{error}");
    // Untouched.
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"something else entirely".to_vec()
    );
}

/// The matching case, and it is the other half of the same rule: a verified
/// archive already on disk is used as-is, so a re-run downloads nothing.
#[test]
fn an_archive_that_matches_its_pin_is_not_downloaded_again() {
    let dir = tempfile::tempdir().unwrap();
    let good = b"the bytes we pinned";
    let artifact = ModelArtifact {
        upstream_size: good.len() as u64,
        ..fixture_row(Box::leak(digest_of(good).into_boxed_str()))
    };
    std::fs::write(dir.path().join(artifact.file_name), good).unwrap();
    let found = ensure_downloaded(&artifact, &options(dir.path())).unwrap();
    assert_eq!(found, dir.path().join(artifact.file_name));
}

/// **An unpinned row is never fetched.** The whole discipline rests on this
/// one refusal; without it, an empty hash column would quietly mean "trust
/// whatever the network sends".
#[test]
fn an_unpinned_row_is_refused_rather_than_fetched() {
    let dir = tempfile::tempdir().unwrap();
    let artifact = fixture_row("");
    assert!(!artifact.is_pinned());
    let error = ensure_downloaded(&artifact, &options(dir.path())).unwrap_err();
    assert!(error.contains("carries no upstream SHA-256"), "{error}");
    assert!(error.contains("voice-models pin"), "{error}");
}

// ---------------------------------------------------------- what is written

/// **The generated catalog parses back through the store's own parser**, which
/// is the only claim that matters: this tool's output is that parser's input.
/// Rendering and re-parsing through the real `Catalog` also proves the flattened
/// descriptor survives the round trip, which is where a hand-assembled JSON
/// would have drifted.
#[test]
fn the_generated_catalog_parses_back_through_the_store() {
    // A distinct digest per row, because two entries sharing one archive is a
    // thing this tool should never produce and not what is under test here.
    let entries: Vec<_> = table::ARTIFACTS
        .iter()
        .enumerate()
        .map(|(index, artifact)| {
            catalog::entry(artifact, 987_654_321, &format!("{index:064x}")).unwrap()
        })
        .collect();
    let document = catalog::catalog(entries.clone()).unwrap();
    let bytes = catalog::render(&document).unwrap();

    let parsed = Catalog::parse(&bytes).expect("the store must be able to read what we publish");
    assert_eq!(parsed, document);
    assert_eq!(parsed.entries.len(), table::ARTIFACTS.len());
    // And the descriptor survives field for field, which is the half a
    // hand-written JSON writer loses first.
    for (published, read_back) in entries.iter().zip(parsed.entries.iter()) {
        assert_eq!(published.descriptor, read_back.descriptor);
        assert_eq!(published.url, read_back.url);
        assert_eq!(published.signature, read_back.signature);
    }
}

/// The per-model key holds one entry, and that entry parses on its own — so a
/// build fetching `models/<id>/<version>.json` needs no catalog.
#[test]
fn the_per_model_document_is_one_catalog_entry() {
    let artifact = &table::ARTIFACTS[0];
    let entry = catalog::entry(artifact, 100, &"c".repeat(64)).unwrap();
    let bytes = catalog::render(&entry).unwrap();
    let parsed: koru_voice_models::CatalogEntry = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed, entry);
    assert_eq!(
        catalog::descriptor_key(artifact),
        format!("models/{}/{}.json", artifact.id, artifact.version)
    );
}

/// The archive key is content-addressed, and the URL in the entry points at it.
/// If those two ever disagree the catalog names bytes that are not there.
#[test]
fn the_entrys_url_is_the_key_the_archive_is_published_under() {
    let artifact = &table::ARTIFACTS[0];
    let digest = "d".repeat(64);
    let entry = catalog::entry(artifact, 100, &digest).unwrap();
    let key = catalog::archive_key(artifact, &digest);
    assert!(key.contains(&digest));
    assert!(key.ends_with(".zip"));
    assert_eq!(
        entry.url.as_str(),
        format!("https://{}/{key}", crate::cloud::HOST)
    );
}

/// **The signature field says it is not one.** Nothing here mints a key, and a
/// placeholder that looked like a signature would be worse than no signature at
/// all — under `TrustPolicy::digest_only` it is stored verbatim beside the
/// installed bytes, so whoever reads it later has to be able to tell.
#[test]
fn the_unsigned_placeholder_cannot_be_mistaken_for_a_signature() {
    assert!(catalog::UNSIGNED.starts_with("UNSIGNED"));
    assert!(!catalog::UNSIGNED.contains("untrusted comment"));
    // A minisign signature is a two-line base64 block with a comment header;
    // this is a sentence, and it is meant to read as one.
    assert!(catalog::UNSIGNED.contains("no Koru model-signing key"));
    assert!(catalog::UNSIGNED.len() > 64);
}

// ----------------------------------------------------------- idempotence

/// **A re-run skips what is already published.** The origin's answer is the
/// parameter, so all four cases are here: same length is a skip, an unknown
/// length is a skip, absent is an upload, and a *different* length under a
/// content-addressed key is a refusal rather than an overwrite.
#[test]
fn a_second_publish_uploads_only_what_is_missing() {
    let key = || "models/x/1/aa.zip".to_string();
    let path = || PathBuf::from("x.zip");

    let skipped = archive_job(key(), path(), 500, Some(Some(500))).unwrap();
    assert!(matches!(skipped.body, Body::Skip("already published")));

    let unknown_length = archive_job(key(), path(), 500, Some(None)).unwrap();
    assert!(matches!(unknown_length.body, Body::Skip(_)));

    let missing = archive_job(key(), path(), 500, None).unwrap();
    assert!(matches!(missing.body, Body::File(_)));
    assert_eq!(missing.cache_control, catalog::ARCHIVE_CACHE);

    let wrong = archive_job(key(), path(), 500, Some(Some(499))).unwrap_err();
    assert!(wrong.contains("cannot both be right"), "{wrong}");
}

/// A document identical to what is already served is skipped; one byte's
/// difference is not.
#[test]
fn an_unchanged_document_is_not_rewritten() {
    let bytes = b"{\"schema_version\":1}\n".to_vec();
    let same = document_job(
        "voice/catalog-v1.json".into(),
        bytes.clone(),
        Some(bytes.clone()),
    );
    assert!(matches!(same.body, Body::Skip("unchanged")));

    let absent = document_job("voice/catalog-v1.json".into(), bytes.clone(), None);
    assert!(matches!(absent.body, Body::Bytes(_)));
    assert_eq!(absent.cache_control, catalog::DOCUMENT_CACHE);

    let changed = document_job("voice/catalog-v1.json".into(), bytes, Some(b"{}".to_vec()));
    assert!(matches!(changed.body, Body::Bytes(_)));
}

/// **The archive is immutable and the descriptor is not**, and that asymmetry
/// is the whole reason the two cache headers differ: the archive's key holds
/// its digest, while a descriptor's `url` and `mirrors` are outside the
/// signature envelope precisely so they can be changed without re-signing.
#[test]
fn only_the_content_addressed_object_is_cached_for_a_year() {
    assert!(catalog::ARCHIVE_CACHE.contains("immutable"));
    assert!(catalog::ARCHIVE_CACHE.contains("31536000"));
    assert!(!catalog::DOCUMENT_CACHE.contains("immutable"));
    assert!(catalog::DOCUMENT_CACHE.contains("no-cache"));
}

// ------------------------------------------------------------------ misc

/// The default work directory is under `dist/`, which `.gitignore` covers.
/// Half a gigabyte per row reaching a commit is the mistake this repeats.
#[test]
fn the_work_directory_is_ignored_by_git() {
    assert!(DEFAULT_WORK.starts_with("dist/"));
    let ignore = include_str!("../../../../.gitignore");
    assert!(ignore.lines().any(|line| line.trim() == "dist"));
    assert!(
        ignore
            .lines()
            .any(|line| line.trim() == "dist/voice-models/")
    );
}

/// An unknown `--only` names what this publisher does know, rather than
/// failing with the id somebody mistyped.
#[test]
fn an_unknown_model_id_is_refused_by_name() {
    let root = Path::new(".");
    let error = Options::parse(root, ["--only", "nope"].iter().map(|s| s.to_string())).unwrap_err();
    assert!(error.contains("no model is called nope"), "{error}");
    assert!(error.contains(table::ARTIFACTS[0].id), "{error}");
}

// --------------------------------------------------------------- helpers

/// The digest a pin would carry, for a fixture built in the test.
fn digest_of(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn zip_names(path: &Path) -> Vec<String> {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    (0..archive.len())
        .map(|index| {
            let entry = archive.by_index(index).unwrap();
            entry.name().unwrap().into_owned()
        })
        .collect()
}

fn zip_entry(path: &Path, name: &str) -> String {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut entry = archive.by_name(name).unwrap();
    let mut out = String::new();
    entry.read_to_string(&mut out).unwrap();
    out
}
