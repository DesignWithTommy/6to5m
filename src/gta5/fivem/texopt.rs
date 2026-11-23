//! Building ytd texture load: the cached DDS as is (original BC format,
//! full size, all mips). The former BC7 -> DXT1 recode lost detail.

use crate::gta5::ytd::{self, Tex};
use std::{fs, path::Path};

/// Tag for ytd fingerprints: bump when the pass changes its output.
pub const TAG: &str = "texopt3";

/// Loads cache texture `name` from `src` (a DDS).
pub fn load(name: &str, src: &Path, _alpha: bool) -> Result<Tex, String> {
    ytd::from_dds(name, &fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?)
}
