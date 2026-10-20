// SPDX-License-Identifier: MPL-2.0

//! Rendering a scene to pixels with no window: the render-to-texture and
//! read-back recipe, once, in production.
//!
//! vello draws into a texture; a window blits that texture to the screen,
//! and this module copies it back to the CPU instead. The recipe lived in
//! four test copies (the canvas, the session view, the voice panel and the
//! socials preview) before it had a caller that ships — a diagram saved as
//! a PNG. Those copies may stay; this is the one the product uses.
//!
//! **Resolution is the caller's.** The texture is `width × height` device
//! pixels and the scene is drawn into it as it is: to render a drawing at
//! 2×, paint it under `Affine::scale(2.0)` and ask for a `2w × 2h` texture.
//! Nothing here knows what logical pixels are.
//!
//! A renderer is built per call through [`build_renderer`](crate::renderer::build_renderer),
//! which is a shader compile and takes tens to hundreds of milliseconds; a
//! caller that already holds a `Renderer` for the context's device — the
//! shell keeps one per device — passes it to [`raster_with`] instead.

#[cfg(test)]
mod tests;

use std::fmt;

use vello::peniko::Color;
use vello::util::{DeviceHandle, RenderContext};
use vello::{AaConfig, RenderParams, Renderer, Scene};

/// Tightly packed RGBA8 pixels, `width × height`, rows unpadded, top row
/// first — what a PNG encoder or a clipboard bitmap takes as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Why no pixels came back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OffscreenError {
    /// A zero width or height: there is nothing to render into.
    Empty,
    /// No GPU adapter answered. A machine that cannot run vello at all; on
    /// one that opens windows this does not happen.
    NoAdapter,
    /// Wider or taller than the device can allocate a texture for.
    TooLarge { width: u32, height: u32, limit: u32 },
    /// vello's renderer could not be built, after the serial retry.
    Renderer(String),
    /// vello refused to render the scene.
    Render(String),
    /// The copy back to the CPU did not complete.
    Readback(String),
}

impl fmt::Display for OffscreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("nothing to render: the size is empty"),
            Self::NoAdapter => f.write_str("no GPU adapter is available"),
            Self::TooLarge {
                width,
                height,
                limit,
            } => write!(
                f,
                "{width} × {height} is larger than the device's {limit} px texture limit"
            ),
            Self::Renderer(detail) => write!(f, "the renderer could not be built: {detail}"),
            Self::Render(detail) => write!(f, "the scene could not be rendered: {detail}"),
            Self::Readback(detail) => write!(f, "the pixels could not be read back: {detail}"),
        }
    }
}

impl std::error::Error for OffscreenError {}

/// Renders `scene` into a `width × height` texture over `background` and
/// reads the pixels back.
///
/// Uses the context's existing device when it has one and creates one
/// otherwise, the same way a window does, then builds a renderer for it
/// and hands over to [`raster_with`]. Blocks the calling thread for the GPU
/// work; the shell calls it from the save worker, never from the loop.
pub fn raster(
    render_cx: &mut RenderContext,
    scene: &Scene,
    width: u32,
    height: u32,
    background: Color,
) -> Result<Rgba, OffscreenError> {
    if width == 0 || height == 0 {
        return Err(OffscreenError::Empty);
    }
    let device_id = pollster::block_on(render_cx.device(None)).ok_or(OffscreenError::NoAdapter)?;
    let handle = &render_cx.devices[device_id];
    let mut renderer =
        crate::renderer::build_renderer(&handle.device).map_err(OffscreenError::Renderer)?;
    raster_with(&mut renderer, handle, scene, width, height, background)
}

/// [`raster`] with a renderer the caller already holds for `handle`'s device.
pub fn raster_with(
    renderer: &mut Renderer,
    handle: &DeviceHandle,
    scene: &Scene,
    width: u32,
    height: u32,
    background: Color,
) -> Result<Rgba, OffscreenError> {
    if width == 0 || height == 0 {
        return Err(OffscreenError::Empty);
    }
    let limit = handle.device.limits().max_texture_dimension_2d;
    if width > limit || height > limit {
        return Err(OffscreenError::TooLarge {
            width,
            height,
            limit,
        });
    }
    let device = &handle.device;
    let queue = &handle.queue;
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("koru-offscreen"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    renderer
        .render_to_texture(
            device,
            queue,
            scene,
            &view,
            &RenderParams {
                base_color: background,
                width,
                height,
                antialiasing_method: AaConfig::Area,
            },
        )
        .map_err(|error| OffscreenError::Render(error.to_string()))?;

    // A buffer copy wants rows padded to 256 bytes; the padding is dropped
    // again below so the result is what an encoder expects.
    let stride = padded_stride(width);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("koru-offscreen-readback"),
        size: u64::from(stride) * u64::from(height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .map_err(|error| OffscreenError::Readback(error.to_string()))?;
    match receiver.try_recv() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(OffscreenError::Readback(error.to_string())),
        Err(_) => {
            return Err(OffscreenError::Readback(
                "the buffer was not mapped after the device was polled".to_owned(),
            ));
        }
    }
    let bytes = {
        let data = slice.get_mapped_range();
        tightly_packed(&data, stride, width, height)
    };
    buffer.unmap();
    Ok(Rgba {
        width,
        height,
        bytes,
    })
}

/// The row stride wgpu requires of a texture-to-buffer copy: `width × 4`
/// rounded up to 256 bytes.
fn padded_stride(width: u32) -> u32 {
    (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

/// The pixels of a padded read-back, with the padding dropped.
fn tightly_packed(padded: &[u8], stride: u32, width: u32, height: u32) -> Vec<u8> {
    let row = (width * 4) as usize;
    let mut bytes = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        let start = y * stride as usize;
        bytes.extend_from_slice(&padded[start..start + row]);
    }
    bytes
}
