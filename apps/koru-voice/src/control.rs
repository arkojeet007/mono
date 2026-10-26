// SPDX-License-Identifier: MPL-2.0

//! **Which endpoint the worker binds.**
//!
//! What crosses it is declared in `koru_voice::control` and
//! `koru_voice::status`, so the Agent can name those messages without linking
//! this crate. The endpoint's token stays here because the contract crate links
//! no socket.

/// `koru_ipc::VOICE`, never a literal: the token reaches a pipe name and a
/// directory name, and a second spelling of it is a client and a worker that
/// each bind something and never meet.
pub const SERVICE: &str = koru_ipc::VOICE;

#[cfg(test)]
mod tests;
