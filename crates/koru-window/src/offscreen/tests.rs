// SPDX-License-Identifier: MPL-2.0

use std::sync::Arc;

use koru_diagram::{Cancel, Limits, TextMeasure};
use koru_draw::diagram::{InterMeasure, PreparedDiagram, paint};
use koru_draw::theme;
use vello::kurbo::Affine;

use super::*;

const FLOWCHART: &str = include_str!("../../../koru-diagram/tests/fixtures/flowchart-showcase.mmd");

/// The padding a 256-byte stride adds is dropped row by row, and nothing
/// else moves: this is the half of the read-back a CPU can check.
#[test]
fn padding_is_dropped_and_rows_stay_in_order() {
    let (width, height) = (3_u32, 2_u32);
    let stride = padded_stride(width);
    assert_eq!(
        stride, 256,
        "12 bytes of pixels round up to one alignment unit"
    );
    let mut padded = vec![0xEE_u8; (stride * height) as usize];
    for y in 0..height as usize {
        for i in 0..(width * 4) as usize {
            padded[y * stride as usize + i] = (y * 16 + i) as u8;
        }
    }
    let bytes = tightly_packed(&padded, stride, width, height);
    assert_eq!(bytes.len(), 24);
    assert_eq!(
        &bytes[..12],
        &(0..12).map(|i| i as u8).collect::<Vec<_>>()[..]
    );
    assert_eq!(
        &bytes[12..],
        &(16..28).map(|i| i as u8).collect::<Vec<_>>()[..]
    );
    assert!(!bytes.contains(&0xEE), "no padding byte survives");
    assert_eq!(padded_stride(64), 256);
    assert_eq!(padded_stride(65), 512);
}

#[test]
fn an_empty_size_is_refused_before_any_gpu_work() {
    let mut context = crate::renderer::context_with(wgpu::Backends::empty());
    assert_eq!(
        raster(&mut context, &Scene::new(), 0, 10, Color::WHITE),
        Err(OffscreenError::Empty)
    );
    assert_eq!(
        raster(&mut context, &Scene::new(), 10, 0, Color::WHITE),
        Err(OffscreenError::Empty)
    );
}

#[test]
fn errors_say_what_happened() {
    assert_eq!(
        OffscreenError::TooLarge {
            width: 9000,
            height: 10,
            limit: 8192
        }
        .to_string(),
        "9000 × 10 is larger than the device's 8192 px texture limit"
    );
    assert_eq!(
        OffscreenError::NoAdapter.to_string(),
        "no GPU adapter is available"
    );
}

/// The flowchart showcase at 2×: the pixels come back at twice the natural
/// size and are not all the background — something was drawn. Skips, saying
/// so, on a machine with no adapter.
#[test]
fn the_flowchart_showcase_rasters_at_two_x() {
    let mut context = crate::renderer::context_with(crate::renderer::default_backends());
    if pollster::block_on(context.device(None)).is_none() {
        eprintln!("skipping: no GPU adapter in this process");
        return;
    }
    let measure: Arc<dyn TextMeasure> = Arc::new(InterMeasure);
    let scene = koru_diagram::layout(FLOWCHART, &measure, &Limits::default(), &Cancel::new())
        .expect("the showcase lays out");
    let prepared = PreparedDiagram::new(Arc::new(scene));
    let natural = prepared.natural_size();
    let palette = &theme::LIGHT;
    let mut drawing = Scene::new();
    paint(&prepared, palette, Affine::scale(2.0), 1.0, &mut drawing);
    let width = (natural.width * 2.0).ceil() as u32;
    let height = (natural.height * 2.0).ceil() as u32;

    let rgba = raster(&mut context, &drawing, width, height, palette.bg).expect("pixels");
    assert_eq!((rgba.width, rgba.height), (width, height));
    assert_eq!(rgba.bytes.len(), (width * height * 4) as usize);
    let bg = palette.bg.to_rgba8();
    let background = [bg.r, bg.g, bg.b, bg.a];
    let (pixels, _) = rgba.bytes.as_chunks::<4>();
    let drawn = pixels.iter().filter(|pixel| **pixel != background).count();
    assert!(drawn > 1000, "{drawn} pixels differ from the background");
    assert!(
        pixels.contains(&background),
        "and the page shows between the boxes"
    );
}
