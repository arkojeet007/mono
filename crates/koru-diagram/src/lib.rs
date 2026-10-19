// SPDX-License-Identifier: MPL-2.0

//! **Mermaid diagrams for Koru: the one crate that links the engine.**
//!
//! A `mermaid` code block is text until something turns it into a picture.
//! This crate is where that happens, and it is the **only** crate in the
//! workspace that depends on `merman` and `merman-render`: the engine is sealed
//! here the way `koru-blocks` seals the syntax highlighter and `koru-spell`
//! seals the spell engine, so it can be replaced without the editor, the host
//! or the MCP server noticing. Its API speaks Koru's vocabulary — the kind of
//! diagram, whether it draws and why not, a layout made of shapes and laid-out
//! labels — and **no merman type crosses it**.
//!
//! This is the skeleton. It exists so merman is in the dependency graph for
//! `cargo deny`, `cargo tree` and the release notices to check, and so the
//! patched `merman-render` is exercised; the smoke tests in `tests.rs` prove
//! the chosen features compile and work. The API follows in a later change.

#[cfg(test)]
mod tests;
