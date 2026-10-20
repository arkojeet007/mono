// SPDX-License-Identifier: MPL-2.0

//! Koru's single OS clipboard mechanism.
//!
//! This crate knows clipboard formats and pixels, not documents, terminal
//! sessions, or paste policy. Callers keep those meanings. The injected
//! [`Backend`] makes every ordinary test independent of the desktop clipboard;
//! [`SystemBackend`] is the only implementation that talks to the OS.

mod system;

#[cfg(test)]
mod tests;

use std::io;
use std::path::{Path, PathBuf};

pub use system::SystemBackend;

/// Stable failure categories. No clipboard content is retained in an error or
/// safe to place in a log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Unavailable,
    CorruptImage,
    OversizedImage,
    EncodeFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: ErrorCode,
}

impl Error {
    pub const fn new(code: ErrorCode) -> Self {
        Self { code }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.code {
            ErrorCode::Unavailable => "clipboard unavailable",
            ErrorCode::CorruptImage => "clipboard image is corrupt",
            ErrorCode::OversizedImage => "clipboard image exceeds the acquisition limit",
            ErrorCode::EncodeFailed => "clipboard image could not be normalized",
        })
    }
}

impl std::error::Error for Error {}

/// Raw interleaved pixels as returned by arboard: exactly four bytes per pixel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaBitmap {
    pub width: usize,
    pub height: usize,
    pub bytes: Vec<u8>,
}

/// A bounded, portable image payload. Consumers never have to understand a
/// platform DIB or retain borrowed clipboard storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PngBitmap {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Bounds are checked before allocation or encoding. The defaults are large
/// enough for a 5K screenshot and deliberately smaller than an unbounded
/// desktop-sized allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageLimits {
    pub max_width: usize,
    pub max_height: usize,
    pub max_pixels: usize,
    pub max_rgba_bytes: usize,
    pub max_png_bytes: usize,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_width: 16_384,
            max_height: 16_384,
            max_pixels: 80_000_000,
            max_rgba_bytes: 320_000_000,
            max_png_bytes: 320_000_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextPayload {
    Text(String),
    /// The clipboard exists but does not currently offer text.
    NonText,
    /// The clipboard could not be opened or queried. A transient lock must not
    /// be mistaken for somebody copying new non-text content.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImagePayload {
    /// Image paths in the platform's clipboard order. Reading file contents is
    /// domain work and remains with the caller.
    Files(Vec<PathBuf>),
    Bitmap(PngBitmap),
    Empty,
}

/// Platform rich formats are raw transport. HTML/RTF/Markdown interpretation
/// remains with the document domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawFormat {
    Html,
    Rtf,
    Markdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawPayload {
    pub format: String,
    pub bytes: Vec<u8>,
}

/// Injectable mechanism boundary. Tests construct one backend per fixture and
/// therefore run in parallel without a process lock or desktop clipboard.
pub trait Backend: Send + Sync {
    fn text(&self) -> Result<Option<String>, Error> {
        Ok(None)
    }

    fn bitmap(&self) -> Result<Option<RgbaBitmap>, Error> {
        Ok(None)
    }

    fn image_paths(&self) -> Result<Vec<PathBuf>, Error> {
        Ok(Vec::new())
    }

    fn raw(&self, _format: RawFormat) -> Result<Option<RawPayload>, Error> {
        Ok(None)
    }

    fn format_names(&self) -> Result<Vec<String>, Error> {
        Ok(Vec::new())
    }

    fn set_text(&self, _text: &str) -> Result<(), Error> {
        Err(Error::new(ErrorCode::Unavailable))
    }

    fn set_text_and_html(&self, plain: &str, _html: &str) -> Result<(), Error> {
        self.set_text(plain)
    }

    fn set_bitmap(&self, _bitmap: &RgbaBitmap) -> Result<(), Error> {
        Err(Error::new(ErrorCode::Unavailable))
    }
}

pub struct Clipboard<B> {
    backend: B,
}

impl<B> Clipboard<B> {
    pub const fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }
}

impl<B: Backend> Clipboard<B> {
    pub fn text(&self) -> TextPayload {
        match self.backend.text() {
            Ok(Some(text)) if !text.is_empty() => TextPayload::Text(text),
            Ok(_) => TextPayload::NonText,
            Err(_) => TextPayload::Unavailable,
        }
    }

    pub fn images(&self, limits: ImageLimits) -> Result<ImagePayload, Error> {
        let paths = self.backend.image_paths();
        if let Ok(paths) = &paths {
            let images: Vec<_> = paths
                .iter()
                .filter(|path| is_image_path(path))
                .cloned()
                .collect();
            if !images.is_empty() {
                return Ok(ImagePayload::Files(images));
            }
        }

        match self.backend.bitmap() {
            Ok(Some(bitmap)) => normalize_png(bitmap, limits).map(ImagePayload::Bitmap),
            Ok(None) if paths.is_ok() => Ok(ImagePayload::Empty),
            Ok(None) | Err(_) => Err(Error::new(ErrorCode::Unavailable)),
        }
    }

    pub fn raw(&self, format: RawFormat) -> Result<Option<RawPayload>, Error> {
        self.backend.raw(format)
    }

    pub fn format_names(&self) -> Result<Vec<String>, Error> {
        self.backend.format_names()
    }

    pub fn set_text(&self, text: &str) -> Result<(), Error> {
        self.backend.set_text(text)
    }

    pub fn set_text_and_html(&self, plain: &str, html: &str) -> Result<(), Error> {
        self.backend.set_text_and_html(plain, html)
    }

    pub fn set_bitmap(&self, bitmap: &RgbaBitmap) -> Result<(), Error> {
        validate_rgba(bitmap, ImageLimits::default())?;
        self.backend.set_bitmap(bitmap)
    }
}

pub fn system() -> Clipboard<SystemBackend> {
    Clipboard::new(SystemBackend)
}

/// Read and normalize a bitmap on a named worker. The completion callback runs
/// on that worker; callers send the typed result into their own event mechanism.
pub fn spawn_image_read(
    limits: ImageLimits,
    complete: impl FnOnce(Result<ImagePayload, Error>) + Send + 'static,
) -> io::Result<std::thread::JoinHandle<()>> {
    spawn_image_read_with(SystemBackend, limits, complete)
}

/// The injectable form of [`spawn_image_read`]. It is public so event-loop
/// owners can prove their handoff without opening the desktop clipboard.
pub fn spawn_image_read_with<B: Backend + 'static>(
    backend: B,
    limits: ImageLimits,
    complete: impl FnOnce(Result<ImagePayload, Error>) + Send + 'static,
) -> io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("koru-clipboard image read".into())
        .spawn(move || complete(Clipboard::new(backend).images(limits)))
}

/// PNG bytes for `width × height` tightly packed RGBA8 pixels — the
/// encoding a clipboard bitmap is normalized to, for a picture Koru rendered
/// itself (a diagram saved as a file, a scene copied as an image).
///
/// Here rather than beside the renderer because this is the crate that
/// already owns a PNG encoder; the pixels arrive as the plain bytes
/// `koru_window::offscreen::raster` returns, so no type crosses between the
/// two. `CorruptImage` when the byte count is not `width × height × 4`.
pub fn png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, Error> {
    let expected = u64::from(width) * u64::from(height) * 4;
    if width == 0 || height == 0 || rgba.len() as u64 != expected {
        return Err(Error::new(ErrorCode::CorruptImage));
    }
    encode_png(width, height, rgba)
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|_| Error::new(ErrorCode::EncodeFailed))?;
    Ok(bytes)
}

fn normalize_png(bitmap: RgbaBitmap, limits: ImageLimits) -> Result<PngBitmap, Error> {
    validate_rgba(&bitmap, limits)?;
    let width = u32::try_from(bitmap.width).map_err(|_| Error::new(ErrorCode::OversizedImage))?;
    let height = u32::try_from(bitmap.height).map_err(|_| Error::new(ErrorCode::OversizedImage))?;
    let bytes = encode_png(width, height, &bitmap.bytes)?;
    if bytes.len() > limits.max_png_bytes {
        return Err(Error::new(ErrorCode::OversizedImage));
    }
    Ok(PngBitmap {
        width,
        height,
        bytes,
    })
}

fn validate_rgba(bitmap: &RgbaBitmap, limits: ImageLimits) -> Result<(), Error> {
    if bitmap.width == 0 || bitmap.height == 0 {
        return Err(Error::new(ErrorCode::CorruptImage));
    }
    let pixels = bitmap
        .width
        .checked_mul(bitmap.height)
        .ok_or_else(|| Error::new(ErrorCode::OversizedImage))?;
    if bitmap.width > limits.max_width
        || bitmap.height > limits.max_height
        || pixels > limits.max_pixels
    {
        return Err(Error::new(ErrorCode::OversizedImage));
    }
    let expected = pixels
        .checked_mul(4)
        .ok_or_else(|| Error::new(ErrorCode::OversizedImage))?;
    if expected > limits.max_rgba_bytes {
        return Err(Error::new(ErrorCode::OversizedImage));
    }
    if bitmap.bytes.len() != expected {
        return Err(Error::new(ErrorCode::CorruptImage));
    }
    Ok(())
}

fn is_image_path(path: &Path) -> bool {
    const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"];
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

// `write_image` is supplied by the trait rather than as an inherent method.
use image::ImageEncoder as _;
