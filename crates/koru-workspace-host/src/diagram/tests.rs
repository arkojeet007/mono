// SPDX-License-Identifier: MPL-2.0

//! The worker over a stub measurer and the real engine on tiny texts; the
//! cache; the *Save as…* name and format rules as pure functions. No disk,
//! no store, no window: a worker here wakes nothing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use koru_diagram::{Counts, FlowchartScene, MeasuredLine, TextMetrics};
use vello::kurbo::Rect;

use super::*;

const FLOWCHART: &str = "flowchart LR\n  A[Start] --> B[Finish]";
const FLOWCHART_LONGER: &str = "flowchart LR\n  A[Start] --> B[Middle] --> C[Finish]";
const SEQUENCE: &str = "sequenceDiagram\n  Ann->>Bo: hello";

/// A gate one measurement can be held at: the first call to `measure`
/// blocks until `release`, and `entered` says it got there.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
    entered: AtomicBool,
    taken: AtomicBool,
}

impl Gate {
    fn hold(&self) {
        // Only the first measurement blocks: the job that runs into the
        // gate is the one the test wants to see superseded.
        if self.taken.swap(true, Ordering::SeqCst) {
            return;
        }
        self.entered.store(true, Ordering::SeqCst);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.opened.wait(open).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }

    fn wait_entered(&self) {
        wait_until(
            || self.entered.load(Ordering::SeqCst),
            "a job reached the gate",
        );
    }
}

/// A deterministic measurer: seven pixels a character, one line per text.
/// Optionally holds its first measurement at a gate, or panics once.
#[derive(Default)]
struct Stub {
    gate: Option<Arc<Gate>>,
    panics_left: AtomicUsize,
}

impl TextMeasure for Stub {
    fn measure(&self, text: &str, _px: f32) -> TextMetrics {
        if let Some(gate) = &self.gate {
            gate.hold();
        }
        if self.panics_left.load(Ordering::SeqCst) > 0 {
            self.panics_left.fetch_sub(1, Ordering::SeqCst);
            panic!("the stub measurer was asked to panic");
        }
        TextMetrics {
            width: 7.0 * text.chars().count() as f32,
            height: 17.0,
            ascent: 13.0,
        }
    }

    fn wrap(&self, text: &str, px: f32, _max_width: f32) -> Vec<MeasuredLine> {
        vec![MeasuredLine {
            text: text.to_owned(),
            width: self.measure(text, px).width,
        }]
    }
}

fn wait_until(mut done: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Drain until `n` replies have arrived, or fail after a generous wait.
fn replies(client: &DiagramClient, n: usize) -> Vec<DiagramReply> {
    let mut got = Vec::new();
    wait_until(
        || {
            got.extend(client.drain());
            got.len() >= n
        },
        &format!("{n} replies"),
    );
    got
}

fn node() -> NodeId {
    NodeId::new()
}

fn worker(stub: Stub) -> DiagramWorker {
    DiagramWorker::start(Arc::new(stub), THREADS, || {})
}

fn empty_prepared() -> Arc<PreparedDiagram> {
    Arc::new(PreparedDiagram::new(Arc::new(DiagramScene {
        kind: Kind::Flowchart,
        bounds: Rect::new(0.0, 0.0, 10.0, 10.0),
        counts: Counts::Flowchart {
            nodes: 0,
            edges: 0,
            groups: 0,
        },
        acc_title: None,
        acc_descr: None,
        body: SceneBody::Flowchart(FlowchartScene {
            nodes: Vec::new(),
            edges: Vec::new(),
            clusters: Vec::new(),
        }),
    })))
}

// ------------------------------------------------------------------ worker

#[test]
fn a_tiny_flowchart_is_laid_out_and_shaped_on_the_worker() {
    let worker = worker(Stub::default());
    let client = worker.client();
    let block = node();
    client.request(block, 1, Arc::from(FLOWCHART));
    let got = replies(&client, 1);
    assert_eq!(got[0].node, block);
    assert_eq!(got[0].text_hash, 1);
    let prepared = got[0].result.as_ref().expect("a drawing");
    let size = prepared.natural_size();
    assert!(size.width > 0.0 && size.height > 0.0, "{size:?}");
    let counters = worker.counters().snapshot();
    assert_eq!(counters.jobs, 1);
    assert_eq!(counters.cache_misses, 1);
    assert_eq!(counters.cancelled, 0);
}

/// Typing supersedes: the running job for a block is cancelled the moment a
/// newer text for it arrives, and only the newest text is answered.
#[test]
fn a_superseded_request_is_cancelled_and_only_the_newest_text_is_answered() {
    let gate = Arc::new(Gate::default());
    let worker = worker(Stub {
        gate: Some(Arc::clone(&gate)),
        ..Stub::default()
    });
    let client = worker.client();
    let block = node();
    client.request(block, 1, Arc::from(FLOWCHART));
    gate.wait_entered();
    // The first job is held mid-layout. A newer text for the same block:
    client.request(block, 2, Arc::from(FLOWCHART_LONGER));
    assert_eq!(
        worker.counters().snapshot().cancelled,
        1,
        "the in-flight job's handle was pulled on the spot"
    );
    // The second thread answers the newest text while the first is held.
    let got = replies(&client, 1);
    assert_eq!(got[0].text_hash, 2);
    assert!(got[0].result.is_ok());
    // Let the held job run on: merman sees the pulled handle at its next
    // measurement and the job ends without a reply.
    gate.release();
    wait_until(
        || worker.counters().snapshot().finished == 2,
        "both jobs to finish",
    );
    assert!(
        client.drain().is_empty(),
        "the cancelled job's answer was never sent"
    );
    assert_eq!(worker.counters().snapshot().jobs, 2);
}

/// The second request for a text this session is answered from the cache,
/// on the spot, with no job — whichever view or block asks.
#[test]
fn a_cache_hit_submits_no_job_and_still_answers() {
    let worker = worker(Stub::default());
    let client = worker.client();
    client.request(node(), 7, Arc::from(FLOWCHART));
    let first = replies(&client, 1);
    assert!(first[0].result.is_ok());
    assert_eq!(worker.cache().len(), 1);

    let other_block = node();
    client.request(other_block, 7, Arc::from(FLOWCHART));
    // Before any thread could have run: the answer is already there.
    let second = client.drain();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].node, other_block);
    assert_eq!(second[0].text_hash, 7);
    assert!(
        Arc::ptr_eq(
            first[0].result.as_ref().unwrap(),
            second[0].result.as_ref().unwrap()
        ),
        "the very same prepared drawing"
    );
    let counters = worker.counters().snapshot();
    assert_eq!(counters.jobs, 1);
    assert_eq!(counters.cache_hits, 1);
    assert_eq!(counters.cache_misses, 1);

    // A second view asks too: shared across clients.
    let another = worker.client();
    another.request(node(), 7, Arc::from(FLOWCHART));
    assert_eq!(another.drain().len(), 1);
    assert_eq!(worker.counters().snapshot().jobs, 1);
}

/// A text that does not draw is answered with its status, and the failure
/// is cached like a drawing: it is a function of the text.
#[test]
fn a_text_that_does_not_draw_is_answered_and_cached() {
    let worker = worker(Stub::default());
    let client = worker.client();
    client.request(node(), 3, Arc::from("pie\n  \"a\": 1"));
    let got = replies(&client, 1);
    assert!(
        matches!(got[0].result, Err(Status::KindNotDrawn { .. })),
        "{:?}",
        got[0].result.as_ref().err()
    );
    client.request(node(), 3, Arc::from("pie\n  \"a\": 1"));
    assert_eq!(client.drain().len(), 1, "answered from the cache");
    assert_eq!(worker.counters().snapshot().jobs, 1);
}

/// A panic inside the engine is a status, not a dead worker: the reply
/// names line 1 with the panic code, and the next job is answered.
#[test]
fn a_panicking_measurer_yields_not_parsed_with_the_panic_code_and_the_worker_goes_on() {
    let worker = worker(Stub {
        gate: None,
        panics_left: AtomicUsize::new(1),
    });
    let client = worker.client();
    client.request(node(), 11, Arc::from(FLOWCHART));
    let got = replies(&client, 1);
    match &got[0].result {
        Err(Status::NotParsed { line, code, .. }) => {
            assert_eq!(*line, 1);
            assert_eq!(code, PANIC_CODE);
        }
        other => panic!("expected a panic status, got {:?}", other.as_ref().err()),
    }
    assert_eq!(worker.counters().snapshot().panicked, 1);

    client.request(node(), 12, Arc::from(FLOWCHART_LONGER));
    let next = replies(&client, 1);
    assert_eq!(next[0].text_hash, 12);
    assert!(next[0].result.is_ok(), "the worker still answers");
}

/// A dropped client takes its replies with it; the worker keeps running for
/// the others.
#[test]
fn a_dropped_client_loses_nothing_but_its_own_replies() {
    let worker = worker(Stub::default());
    let gone = worker.client();
    gone.request(node(), 21, Arc::from(FLOWCHART));
    drop(gone);
    let alive = worker.client();
    alive.request(node(), 22, Arc::from(SEQUENCE));
    let got = replies(&alive, 1);
    assert_eq!(got[0].text_hash, 22);
    wait_until(
        || worker.counters().snapshot().finished == 2,
        "both jobs to finish",
    );
}

// ---------------------------------------------------------------- mapping

#[test]
fn a_timeout_maps_to_the_uncounted_too_large_of_the_text_kind() {
    let counters = Counters::default();
    assert_eq!(
        map_fail(FLOWCHART, Err(Fail::TimedOut), &counters)
            .unwrap()
            .err(),
        Some(Status::TooLarge {
            kind: Kind::Flowchart,
            count: 0
        })
    );
    assert_eq!(
        map_fail(SEQUENCE, Err(Fail::TimedOut), &counters)
            .unwrap()
            .err(),
        Some(Status::TooLarge {
            kind: Kind::Sequence,
            count: 0
        })
    );
    // No header at all: the flowchart words, which is what the block most
    // likely was.
    assert_eq!(
        map_fail("not a diagram", Err(Fail::TimedOut), &counters)
            .unwrap()
            .err(),
        Some(Status::TooLarge {
            kind: Kind::Flowchart,
            count: 0
        })
    );
}

#[test]
fn every_other_failure_maps_as_the_editor_expects() {
    let counters = Counters::default();
    assert!(map_fail(FLOWCHART, Err(Fail::Cancelled), &counters).is_none());
    assert_eq!(
        map_fail(FLOWCHART, Err(Fail::Status(Status::Empty)), &counters)
            .unwrap()
            .err(),
        Some(Status::Empty)
    );
    let internal = map_fail(
        FLOWCHART,
        Err(Fail::Internal {
            code: "koru.diagram.layout".into(),
            message: "merman gave no layout".into(),
        }),
        &counters,
    )
    .unwrap()
    .err();
    assert_eq!(
        internal,
        Some(Status::NotParsed {
            line: 1,
            message: "merman gave no layout".into(),
            code: "koru.diagram.layout".into(),
        })
    );
    assert_eq!(
        counters.snapshot().panicked,
        0,
        "only the panic code counts"
    );
    let _ = map_fail(
        FLOWCHART,
        Err(Fail::Internal {
            code: PANIC_CODE.into(),
            message: "boom".into(),
        }),
        &counters,
    );
    assert_eq!(counters.snapshot().panicked, 1);
}

// ------------------------------------------------------------------- cache

#[test]
fn the_cache_evicts_the_least_recently_used_past_the_entry_bound() {
    let cache = DiagramCache::default();
    for hash in 0..CACHE_ENTRIES as u64 {
        cache.insert(hash, &Err(Status::Empty));
    }
    assert_eq!(cache.len(), CACHE_ENTRIES);
    // Touch the oldest so it is no longer the one to go.
    assert!(cache.get(0).is_some());
    cache.insert(CACHE_ENTRIES as u64, &Err(Status::Empty));
    assert_eq!(cache.len(), CACHE_ENTRIES);
    assert!(cache.get(0).is_some(), "recently used: kept");
    assert!(cache.get(1).is_none(), "least recently used: evicted");
    assert!(cache.get(CACHE_ENTRIES as u64).is_some());
}

#[test]
fn the_cache_counts_bytes_by_estimate_and_replaces_in_place() {
    let cache = DiagramCache::default();
    let drawing: Outcome = Ok(empty_prepared());
    cache.insert(1, &drawing);
    assert_eq!(cache.bytes(), estimate(&drawing));
    cache.insert(1, &Err(Status::Empty));
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.bytes(), estimate(&Err(Status::Empty)));
    assert!(estimate(&Err(Status::Empty)) < estimate(&drawing) + LINE_BYTES);
}

// ----------------------------------------------------------------- save as…

#[test]
fn the_typed_extension_decides_the_format() {
    let svg = |name: &str| {
        let (path, format) = save_target(PathBuf::from(name));
        assert_eq!(format, SaveFormat::Svg, "{name}");
        assert_eq!(path, PathBuf::from(name), "the typed name is kept");
    };
    svg("a.svg");
    svg("A.SVG");
    svg("/tmp/pictures/flow.Svg");

    let png = |name: &str, final_name: &str| {
        let (path, format) = save_target(PathBuf::from(name));
        assert_eq!(format, SaveFormat::Png, "{name}");
        assert_eq!(path, PathBuf::from(final_name), "{name}");
    };
    png("a.png", "a.png");
    png("a", "a.png");
    png("a.jpeg", "a.jpeg");
    png("/tmp/pictures/flow", "/tmp/pictures/flow.png");
    png("A.PNG", "A.PNG");
}

#[test]
fn the_default_name_is_the_page_title_as_a_png_or_diagram_png() {
    assert_eq!(default_file_name("Release plan"), "Release plan.png");
    assert_eq!(default_file_name("  padded  "), "padded.png");
    assert_eq!(default_file_name("a/b:c?"), "a-b-c.png");
    assert_eq!(default_file_name(""), "Diagram.png");
    assert_eq!(default_file_name("   "), "Diagram.png");
    assert_eq!(default_file_name("???"), "Diagram.png");
}

#[test]
fn a_payload_encodes_to_the_file_it_names() {
    assert_eq!(
        SavePayload::Svg("<svg/>".into()).encode().unwrap(),
        b"<svg/>".to_vec()
    );
    let png = SavePayload::Rgba(koru_window::offscreen::Rgba {
        width: 1,
        height: 1,
        bytes: vec![0, 0, 0, 255],
    })
    .encode()
    .unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert!(
        SavePayload::Rgba(koru_window::offscreen::Rgba {
            width: 2,
            height: 1,
            bytes: vec![0; 4],
        })
        .encode()
        .is_err(),
        "a size its pixels do not fill is refused, not written"
    );
}

#[test]
fn a_save_request_prints_no_title_and_compares_by_drawing() {
    let prepared = empty_prepared();
    let request = SaveDiagram {
        prepared: Arc::clone(&prepared),
        title: "A secret title".into(),
    };
    let printed = format!("{request:?}");
    assert!(!printed.contains("secret"), "{printed}");
    assert_eq!(
        request,
        SaveDiagram {
            prepared,
            title: "A secret title".into()
        }
    );
    assert_ne!(
        request,
        SaveDiagram {
            prepared: empty_prepared(),
            title: "A secret title".into()
        }
    );
}
