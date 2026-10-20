// SPDX-License-Identifier: MPL-2.0

//! The window layer: colour, type, geometry, and putting marks on a scene.
//!
//! **Nothing here knows what a node is.** That is the whole point of the crate
//! boundary rather than a module: calendar and kanban are the next two views and
//! neither is a text editor, so they want the palette, the glyph pipeline and
//! the geometry without inheriting an editor to get them.
//!
//! The rule is enforced by the manifest — this crate does not depend on
//! `koru-editor`, so it *cannot* reach for a `Paragraph`. If something here
//! starts needing one, it belongs a layer up.

pub mod anim;
pub mod brand;
pub mod checkbox;
pub mod chip;
pub mod diagram;
pub mod emoji;
pub mod geometry;
pub mod icons;
pub mod image;
pub mod inspect;
pub mod pill;
pub mod scene;
pub mod text;
pub mod theme;
pub mod tile;
pub mod tween;

pub use emoji::split_leading_emoji;
pub use image::ImageCache;
