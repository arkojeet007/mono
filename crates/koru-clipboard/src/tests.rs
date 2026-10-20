// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The bytes `png` writes decode back to the same size and pixels: what a
/// saved diagram is, and what the clipboard's own normalization relies on.
#[test]
fn png_round_trips_its_pixels() {
    let (width, height) = (3_u32, 2_u32);
    let rgba: Vec<u8> = (0..width * height * 4).map(|i| (i * 7) as u8).collect();
    let bytes = png(width, height, &rgba).expect("encodes");
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "a PNG signature");
    let decoded = image::load_from_memory(&bytes).expect("decodes").to_rgba8();
    assert_eq!(decoded.dimensions(), (width, height));
    assert_eq!(decoded.into_raw(), rgba);
}

/// A byte count that is not `width × height × 4` is refused rather than
/// encoded as a picture of the wrong rows.
#[test]
fn png_refuses_a_byte_count_that_does_not_match() {
    assert_eq!(
        png(2, 2, &[0; 15]).unwrap_err().code,
        ErrorCode::CorruptImage
    );
    assert_eq!(png(0, 2, &[]).unwrap_err().code, ErrorCode::CorruptImage);
}
