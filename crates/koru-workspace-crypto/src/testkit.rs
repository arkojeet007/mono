// SPDX-License-Identifier: MPL-2.0

//! Test-only helpers. Compiled out of every shipped build.
//!
//! The one thing here that deserves a note is [`VectorRng`]. Golden vectors
//! have to be **reproducible**: an HPKE encapsulation draws an ephemeral key
//! pair, and a chunked seal draws a salt, so a vector generated from the OS
//! CSPRNG could never be regenerated and reviewed against its predecessor. RFC
//! 9180 solves the same problem by publishing `ikmE` alongside every vector;
//! this solves it by making the randomness a parameter and feeding it a
//! deterministic stream.
//!
//! **It is a test fixture and nothing else.** It is not exported, not reachable
//! from the shipped crate, and its output is a BLAKE3 extendable-output stream
//! from a fixed seed — which is a fine PRF and a terrible source of secrets,
//! because the seed is in the source file.

use core::convert::Infallible;

use rand_core::{TryCryptoRng, TryRng};

pub struct VectorRng {
    reader: blake3::OutputReader,
}

impl VectorRng {
    pub fn new(seed: &[u8]) -> Self {
        let mut hasher =
            blake3::Hasher::new_derive_key("koru.do/workspace-crypto/v1 golden vector rng");
        hasher.update(seed);
        Self {
            reader: hasher.finalize_xof(),
        }
    }
}

// `rand_core` 0.10 derives `Rng` and `CryptoRng` from the infallible `Try*`
// traits, so the reader is implemented once, there.
impl TryRng for VectorRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        let mut bytes = [0u8; 4];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        let mut bytes = [0u8; 8];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        self.reader.fill(dst);
        Ok(())
    }
}

impl TryCryptoRng for VectorRng {}

pub fn unhex(s: &str) -> Vec<u8> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(cleaned.len().is_multiple_of(2), "hex must be even length");
    (0..cleaned.len() / 2)
        .map(|i| u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16).expect("hex"))
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
