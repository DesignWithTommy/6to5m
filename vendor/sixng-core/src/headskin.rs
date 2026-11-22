//! Head skins: face textures for ped heads.
//!
//! Head drawables bind no textures; the game picks the face from material
//! presets in the packs.rpf meta resources (0x7a). A preset (`f795e8ba`) has
//! a tag map (`12f85293`, key -> u32 list). The tag `4bfd27dd` lists the
//! drawables (joaat of the drawable name) the preset applies to, so a head
//! listed there uses that preset. Its "base" parameter (`3eaddcd7` =
//! joaat("base")) holds the face albedo in slot `ead733fc` and a second
//! texture (normal map) in slot `d689b17f`.
//!
//! Heads that no preset lists (most generic `a_head_base_*`, crowd `hc_*`
//! heads whose presets have empty slots) get an automatic skin: a face from
//! the linked heads of the same sex, body type and ethnicity code (`fr1_lat`
//! in `head_fr1_lat_018_m_full`), preferring the same age letter.
//!
//! The table is built once from the packs.rpf index (a few seconds) and
//! cached as `<work>/index/head_skins.json`.

use crate::meta7a::{joaat, Meta};
use crate::resource::Resource;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

const PRESET: u32 = 0xf795_e8ba;
const TAG_DRAWABLE: u32 = 0x4bfd_27dd;
const PARAM_BASE: u32 = 0x44e2_1c90;
const CACHE_VERSION: u32 = 2;

/// Textures of one face preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Skin {
    pub albedo: u64,
    /// Second texture of the "base" parameter (a normal map), 0 if none.
    pub normal: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct HeadSkins {
    version: u32,
    /// joaat(head drawable name) -> face presets that list it.
    pub by_head: BTreeMap<u32, Vec<Skin>>,
    /// Linked faces by head code (`fr1_lat`), with the age letter.
    pub library: BTreeMap<String, Vec<(char, Skin)>>,
}

/// Choice made for one head.
#[derive(Debug, Clone, Serialize)]
pub struct Choice {
    pub skin: Skin,
    /// "linked" (a preset lists this head) or "auto".
    pub source: &'static str,
    /// Code the automatic choice matched (`fr1_lat`, `f`, or `any`).
    pub group: String,
    pub index: usize,
    pub count: usize,
}

/// Head code of a drawable name: (sex + body type + ethnicity, age letter),
/// e.g. `cs_jada_watts_head_fr1_afr_000_y_full` -> ("fr1_afr", 'y').
pub fn head_code(name: &str) -> Option<(String, char)> {
    let p: Vec<&str> = name.split('_').collect();
    let i = p.iter().position(|s| *s == "head")?;
    let rest = &p[i + 1..];
    // head_<sex/body>_<eth>_<nnn>_<age> or a_head_base_<sex/body>_<eth>_...
    let rest = if rest.first() == Some(&"base") { &rest[1..] } else { rest };
    let body = rest.first()?;
    if body.len() != 3 || !matches!(body.as_bytes()[0], b'f' | b'm') {
        return None;
    }
    let eth = rest.get(1).filter(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_lowercase())).copied();
    let age = rest.iter().skip(2).find(|s| s.len() == 1).and_then(|s| s.chars().next()).unwrap_or('m');
    Some((match eth {
        Some(e) => format!("{body}_{e}"),
        None => body.to_string(),
    }, age))
}

/// True for a head drawable (not eyes, lashes or other head parts).
pub fn is_head(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    (n.starts_with("head_") || n.contains("_head_")) && !["eye", "lash", "teeth", "tongue", "hair", "brow", "beard", "tattoo"].iter().any(|k| n.contains(k)) && head_code(&n).is_some()
}

fn build(packs: &Path, ix: &crate::scan::ArchiveIndex) -> HeadSkins {
    let names: HashMap<u32, &str> = ix.entries.iter().filter_map(|e| e.name.as_deref()).map(|n| (joaat(n), n)).collect();
    let found: Vec<(u32, Skin)> = ix
        .entries
        .par_iter()
        .filter(|e| e.sub == "7a")
        .flat_map_iter(|e| {
            let mut v = Vec::new();
            let Ok(r) = Resource::load_meta(packs, e) else { return v };
            let Some(m) = Meta::parse(&r) else { return v };
            // Field keys as the meta decoder names them (hex unless known).
            let k = |h: u32| crate::meta7a::field_name(h);
            for o in m.objects(PRESET) {
                let Some(p) = m.object(PRESET, o, 1) else { continue };
                let heads: Vec<u32> = p[k(0x12f8_5293).as_str()]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|t| t[k(0x6098_a50e).as_str()].as_u64() == Some(TAG_DRAWABLE as u64))
                    .flat_map(|t| t[k(0x063f_a3f2).as_str()].as_array().cloned().unwrap_or_default())
                    .filter_map(|x| x.as_u64().map(|x| x as u32))
                    .collect();
                if heads.is_empty() {
                    continue;
                }
                let hx = |v: &serde_json::Value| v.as_str().and_then(|s| u64::from_str_radix(s, 16).ok()).unwrap_or(0);
                for prm in p[k(0xebac_f18c).as_str()].as_array().into_iter().flatten() {
                    if prm[k(0x3ead_dcd7).as_str()].as_u64() != Some(PARAM_BASE as u64) {
                        continue;
                    }
                    let t = &prm[k(0xe7cb_9e1b).as_str()];
                    let s = Skin { albedo: hx(&t[k(0xead7_33fc).as_str()]), normal: hx(&t[k(0xd689_b17f).as_str()]) };
                    if s.albedo != 0 {
                        v.extend(heads.iter().map(|&h| (h, s)));
                    }
                }
            }
            v
        })
        .collect();
    let mut by: BTreeMap<u32, BTreeSet<Skin>> = BTreeMap::new();
    for (h, s) in found {
        by.entry(h).or_default().insert(s);
    }
    let mut library: BTreeMap<String, BTreeSet<(char, Skin)>> = BTreeMap::new();
    for (h, ss) in &by {
        let Some(n) = names.get(h) else { continue };
        if !is_head(n) {
            continue;
        }
        if let Some((code, age)) = head_code(n) {
            library.entry(code).or_default().extend(ss.iter().map(|s| (age, *s)));
        }
    }
    HeadSkins {
        version: CACHE_VERSION,
        by_head: by.into_iter().map(|(h, s)| (h, s.into_iter().collect())).collect(),
        library: library.into_iter().map(|(c, s)| (c, s.into_iter().collect())).collect(),
    }
}

/// The head-skin table (built from the packs.rpf index on first use, then
/// cached in the work directory).
pub fn get(game_root: &Path) -> Result<&'static HeadSkins, String> {
    static T: OnceLock<Result<HeadSkins, String>> = OnceLock::new();
    T.get_or_init(|| {
        let work = crate::names::work_dir().ok_or("no work directory")?;
        let cache = work.join("index").join("head_skins.json");
        let ixp = crate::scan::index_path(&work.join("index"), "packs.rpf");
        let stamp = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        if let Ok(b) = std::fs::read(&cache) {
            if let Ok(t) = serde_json::from_slice::<HeadSkins>(&b) {
                if t.version == CACHE_VERSION && stamp(&cache) >= stamp(&ixp) {
                    return Ok(t);
                }
            }
        }
        let ix = crate::scan::load_index(&ixp).map_err(|e| format!("packs.rpf index ({}): {e}; index packs.rpf first", ixp.display()))?;
        let t = build(&game_root.join("packs.rpf"), &ix);
        if t.by_head.is_empty() {
            return Err("no face presets found in packs.rpf".into());
        }
        if let Ok(b) = serde_json::to_vec(&t) {
            let _ = std::fs::write(&cache, b);
        }
        Ok(t)
    })
    .as_ref()
    .map_err(|e| e.clone())
}

impl HeadSkins {
    /// Candidate skins for a head (linked ones, else the automatic group),
    /// filtered by `exists` (the texture is in a store), and the choice at
    /// `index` (wrapping).
    pub fn choose(&self, name: &str, index: usize, exists: impl Fn(u64) -> bool) -> Option<Choice> {
        let n = name.to_ascii_lowercase();
        let linked: Vec<Skin> = self.by_head.get(&joaat(&n)).into_iter().flatten().copied().filter(|s| exists(s.albedo)).collect();
        if !linked.is_empty() {
            let i = index % linked.len();
            return Some(Choice { skin: linked[i], source: "linked", group: String::new(), index: i, count: linked.len() });
        }
        let (code, age) = head_code(&n)?;
        let sex = &code[..1];
        let pick = |pred: &dyn Fn(&str, char) -> bool| -> Vec<Skin> {
            let mut v: Vec<Skin> = self.library.iter().flat_map(|(c, l)| l.iter().filter(move |(a, _)| pred(c, *a)).map(|(_, s)| *s)).filter(|s| exists(s.albedo)).collect();
            v.sort();
            v.dedup();
            v
        };
        let tries: [(&dyn Fn(&str, char) -> bool, String); 4] = [
            (&|c, a| c == code && a == age, code.clone()),
            (&|c, _| c == code, code.clone()),
            (&|c, _| c.starts_with(sex), sex.to_string()),
            (&|_, _| true, "any".into()),
        ];
        for (pred, group) in tries.iter() {
            let v = pick(*pred);
            if !v.is_empty() {
                let i = index % v.len();
                return Some(Choice { skin: v[i], source: "auto", group: group.clone(), index: i, count: v.len() });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(head_code("cs_jada_watts_head_fr1_afr_000_y_full"), Some(("fr1_afr".into(), 'y')));
        assert_eq!(head_code("a_head_base_fo1_000_m_full"), Some(("fo1".into(), 'm')));
        assert_eq!(head_code("head_mr1_lat_118_m_full"), Some(("mr1_lat".into(), 'm')));
        assert!(is_head("hc_colombians_head_mr1_lat_004_m"));
        assert!(!is_head("cs_jada_watts_head_fr1_afr_000_y_eyes_full"));
        assert!(!is_head("ambient_femalebody_head"));
    }
}
