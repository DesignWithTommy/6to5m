//! Building ytd texture fingerprint tag. Textures ship as decoded (original
//! BC format, full size, all mips); the former BC7 -> DXT1 recode lost detail.

/// Tag for ytd fingerprints: bump when the pass changes its output.
pub const TAG: &str = "texopt3";
