//! Shared ped skeleton for skinning ped clothing.
//!
//! Ped clothing drawables carry only their own `GM_U01_*` girth helper
//! bones. Their vertex joint indices go through the model's bone palette
//! (`drawable::Model::bone_palette`, sorted joaat name hashes) to bones that
//! are either those helpers or bones of the shared ped skeleton. That
//! skeleton (596 bones, `SKEL_ROOT` …) is in the `ambient_femalebody` /
//! `ambient_malebody` fragments of `packs.rpf`; it is loaded once from the
//! packs index in the work directory.
use crate::drawable::{find_skeleton, Bone};
use crate::meta7a::joaat;
use crate::resource::Resource;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

pub struct PedSkeleton {
    /// Name of the fragment the skeleton came from.
    pub source: String,
    pub bones: Vec<Bone>,
    /// joaat(bone name) -> index into `bones`.
    pub by_hash: HashMap<u32, usize>,
}

/// Fragment holding the reference skeleton for a body (female or male).
pub fn source_name(female: bool) -> &'static str {
    if female {
        "ambient_femalebody"
    } else {
        "ambient_malebody"
    }
}

fn load(game_root: &Path, female: bool) -> Option<PedSkeleton> {
    let work = crate::names::work_dir()?;
    let ix = crate::scan::load_index(&crate::scan::index_path(&work.join("index"), "packs.rpf")).ok()?;
    let name = source_name(female);
    let e = ix.entries.iter().find(|e| e.name.as_deref() == Some(name))?;
    let r = Resource::load(&game_root.join("packs.rpf"), e).ok()?;
    let bones = find_skeleton(&r);
    if bones.is_empty() || bones.iter().any(|b| b.bind_matrix.is_none()) {
        return None;
    }
    let by_hash = bones.iter().enumerate().map(|(i, b)| (joaat(&b.name), i)).collect();
    Some(PedSkeleton { source: name.to_string(), bones, by_hash })
}

/// The shared ped skeleton (cached), or None when packs.rpf is not indexed
/// in the work directory or the fragment is missing.
pub fn get(game_root: &Path, female: bool) -> Option<&'static PedSkeleton> {
    static F: OnceLock<Option<PedSkeleton>> = OnceLock::new();
    static M: OnceLock<Option<PedSkeleton>> = OnceLock::new();
    let cell = if female { &F } else { &M };
    cell.get_or_init(|| load(game_root, female)).as_ref()
}

/// Female rig unless the name has the male rig token (`_mr1`).
pub fn is_female_name(name: &str) -> bool {
    !name.contains("_mr1")
}
