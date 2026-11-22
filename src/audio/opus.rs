//! Opus decoding with the pure-Rust `opus-decoder` crate (no libopus, no DLL).

use std::path::{Path, PathBuf};

const MAX_FRAME: usize = 5760;

/// Kept for the call sites: decoding is always available, so this returns a
/// placeholder path.
pub fn find(_explicit: Option<&Path>) -> Option<PathBuf> {
    Some(PathBuf::from("opus-decoder (built in)"))
}

pub fn version(_dll: &Path) -> Result<String, String> {
    Ok("opus-decoder 0.1 (pure Rust, built in)".into())
}

pub struct Decoder {
    dec: opus_decoder::OpusDecoder,
    channels: usize,
    pcm: Vec<i16>,
}

impl Decoder {
    pub fn new(_dll: &Path, channels: u8) -> Result<Self, String> {
        let dec = opus_decoder::OpusDecoder::new(48_000, channels.into()).map_err(|e| format!("opus: {e:?}"))?;
        Ok(Self { dec, channels: channels.into(), pcm: vec![0; MAX_FRAME * usize::from(channels)] })
    }

    /// Decode one packet to interleaved 16-bit PCM at 48 kHz (empty on error).
    pub fn decode(&mut self, packet: &[u8]) -> &[i16] {
        match self.dec.decode(packet, &mut self.pcm, false) {
            Ok(n) => &self.pcm[..n * self.channels],
            Err(_) => &[],
        }
    }
}
