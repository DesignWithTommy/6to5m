//! Ped wardrobe: the parts of a ped's outfit from the packs.rpf wardrobe
//! meta resources, ported from SixNG `wardrobe.rs` (upstream 3890537,
//! GPL-3.0) and GTA6-Asset-Info 12-materials "How wardrobe items link to
//! drawables" (3e8028c).
//!
//! Records link by u32 id, not by pointer: a variation (`a9d57751`) lists
//! items (`fb5bb20d` {`65121749` item id}); an item (`f795e8ba`) has a parent
//! item (`080eec39` / `e13ebaf8`, a colour variant points at its base) and
//! texture sets (`ebacf18c` list of `37e52277`, 64-bit texture hashes). An
//! item's drawable is the model that uses most of the textures of the item
//! and its parents.
//!
//! The ped's own drawables (`<name>_*`) give seed textures; the variation
//! sharing most of them is the ped's outfit, and each of its items resolves
//! to the drawable with the most shared textures (among drawables of the
//! same fit tag, `fr1` / `mr1`). Cached per ped in
//! `<work>/fivem_wardrobe/<name>.json`.

use rayon::prelude::*;
use serde_json::Value;
use sixng_core::meta7a::{field_name as f, Meta};
use sixng_core::resource::Resource;
use sixng_core::scan::{ArchiveIndex, Entry};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

const CACHE_V: u32 = 2;
const SLOT_DIFFUSE: u32 = 0xC8E8_C282;
const SLOT_BUMP: u32 = 0x1E77_BF4C;

/// One wardrobe item: textures of it and its parents, and its first texture
/// set's (diffuse, normal) for the drawable's texture overrides (SixNG
/// 28c0129 `set_textures`).
struct Item {
    tex: Vec<u64>,
    set: Vec<(u32, u64)>,
}

fn hashes64(v: &Value, out: &mut Vec<u64>) {
    match v {
        Value::Array(a) => a.iter().for_each(|x| hashes64(x, out)),
        Value::Object(m) => m.values().for_each(|x| hashes64(x, out)),
        Value::String(s) if s.len() == 16 => out.extend(u64::from_str_radix(s, 16).ok().filter(|&h| h != 0)),
        _ => {}
    }
}

/// (parameter slot, 64-bit texture hash) of every texture a drawable binds.
fn drawable_slots(packs: &Path, e: &Entry) -> Vec<(u32, u64)> {
    let Ok(r) = Resource::load(packs, e) else { return Vec::new() };
    sixng_core::drawable::find_shaders(&r)
        .iter()
        .flat_map(|s| s.textures.iter())
        .filter_map(|t| Some((u32::from_str_radix(t.param_hash.trim_start_matches("0x"), 16).ok()?, u64::from_str_radix(t.hash64.trim_start_matches("0x"), 16).ok().filter(|&h| h != 0)?)))
        .collect()
}

/// 64-bit texture hashes a drawable's shaders bind.
fn drawable_textures(packs: &Path, e: &Entry) -> HashSet<u64> {
    drawable_slots(packs, e).into_iter().map(|x| x.1).collect()
}

/// First texture set along the chain with a diffuse (`ead733fc`) or normal
/// (`Normal`) texture.
fn set_textures(chain: &[&Value]) -> Vec<(u32, u64)> {
    for c in chain {
        for s in c[f(0xebac_f18c)].as_array().into_iter().flatten() {
            let e = &s[f(0xe7cb_9e1b)];
            let v: Vec<(u32, u64)> = [(SLOT_DIFFUSE, e[f(0xead7_33fc)].as_str()), (SLOT_BUMP, e["Normal"].as_str())]
                .into_iter()
                .filter_map(|(k, x)| Some((k, u64::from_str_radix(x?, 16).ok().filter(|&h| h != 0)?)))
                .collect();
            if !v.is_empty() {
                return v;
            }
        }
    }
    Vec::new()
}

/// Texture sets of every item of every variation in one wardrobe resource:
/// per variation, per item, the textures of the item and its parent chain.
fn variations(m: &Meta) -> Vec<Vec<Item>> {
    let get = |h: u32| -> Vec<Value> { m.objects(h).into_iter().filter_map(|o| m.object(h, o, 0)).collect() };
    let items: HashMap<u64, Value> = get(0xf795_e8ba).into_iter().filter_map(|v| Some((v[f(0x6512_1749)].as_u64()?, v))).collect();
    let mut out = Vec::new();
    for v in get(0xa9d5_7751) {
        let mut parts = Vec::new();
        for ln in v[f(0xfb5b_b20d)].as_array().into_iter().flatten() {
            let Some(it) = ln[f(0x6512_1749)].as_u64().and_then(|i| items.get(&i)) else { continue };
            let mut chain = vec![it];
            while chain.len() < 8 {
                let last = chain[chain.len() - 1];
                let next = [0x080e_ec39u32, 0xe13e_baf8].iter().filter_map(|&k| last[f(k)].as_u64()).filter(|&x| x != 0).filter_map(|x| items.get(&x)).find(|n| !chain.iter().any(|c| std::ptr::eq(*c, *n)));
                match next {
                    Some(n) => chain.push(n),
                    None => break,
                }
            }
            let mut tex = Vec::new();
            chain.iter().for_each(|c| hashes64(c, &mut tex));
            tex.sort_unstable();
            tex.dedup();
            if !tex.is_empty() {
                parts.push(Item { tex, set: set_textures(&chain) });
            }
        }
        if !parts.is_empty() {
            out.push(parts);
        }
    }
    out
}

/// The ped's outfit: drawable names of its wardrobe variation, and texture
/// overrides (drawable's own texture -> the item's, where the drawable binds
/// one texture in that slot; SixNG 28c0129 `tex_remap`). None when no
/// variation shares textures with the ped's own drawables.
pub type Remap = Vec<(u64, u64)>;
pub fn parts(game: &Path, work: &Path, ix: &ArchiveIndex, name: &str, fit: &str) -> Option<(Vec<String>, Remap)> {
    let cache = work.join("fivem_wardrobe").join(format!("{name}.json"));
    if let Ok(v) = fs::read(&cache).map_err(|_| ()).and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|_| ())) {
        if v["v"] == CACHE_V {
            let p: Vec<String> = v["parts"].as_array()?.iter().filter_map(|x| x.as_str().map(String::from)).collect();
            let rm: Remap = v["remap"].as_array().into_iter().flatten().filter_map(|x| Some((u64::from_str_radix(x[0].as_str()?, 16).ok()?, u64::from_str_radix(x[1].as_str()?, 16).ok()?))).collect();
            return (!p.is_empty()).then_some((p, rm));
        }
    }
    let packs = game.join("packs.rpf");
    let t = std::time::Instant::now();
    let dr: Vec<&Entry> = ix.entries.iter().filter(|e| e.rtype.as_deref() == Some("dr") && e.name.is_some()).collect();
    let own: Vec<&Entry> = dr.iter().copied().filter(|e| e.name.as_deref().is_some_and(|n| n.to_ascii_lowercase().starts_with(&format!("{name}_")))).collect();
    let seed: HashSet<u64> = own.par_iter().flat_map_iter(|e| drawable_textures(&packs, e)).collect();
    if seed.is_empty() {
        return None;
    }
    // the wardrobe variation sharing most textures with the ped's drawables
    let best = ix
        .entries
        .par_iter()
        .filter(|e| e.sub == "7a")
        .filter_map(|e| {
            let r = Resource::load_meta(&packs, e).ok()?;
            let m = Meta::parse(&r)?;
            variations(&m).into_iter().map(|v| (v.iter().flat_map(|i| &i.tex).filter(|h| seed.contains(h)).count(), e.id.clone(), v)).max_by_key(|x| x.0)
        })
        .filter(|x| x.0 > 0)
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut remap: Remap = Vec::new();
    let result: Vec<String> = match &best {
        Some((_, _, v)) => {
            // drawables of the same fit tag (and the ped's own), by shared textures
            let pool: Vec<(String, Vec<(u32, u64)>)> = dr
                .par_iter()
                .filter(|e| e.name.as_deref().is_some_and(|n| {
                    let n = n.to_ascii_lowercase();
                    n.starts_with(&format!("{name}_")) || n.split('_').any(|t| t == fit)
                }))
                .map(|e| (e.name.clone().unwrap_or_default().to_ascii_lowercase(), drawable_slots(&packs, e)))
                .filter(|x| !x.1.is_empty())
                .collect();
            let mut out = Vec::new();
            for it in v {
                let tex = &it.tex;
                let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
                for (n, ts) in &pool {
                    let c = tex.iter().filter(|h| ts.iter().any(|t| t.1 == **h)).count();
                    if c > 0 {
                        counts.insert(n, c);
                    }
                }
                // most textures, then `_full` before cull / LOD copies, then name
                if let Some((n, _)) = counts.iter().max_by(|a, b| a.1.cmp(b.1).then(a.0.ends_with("_full").cmp(&b.0.ends_with("_full"))).then(b.0.cmp(a.0))) {
                    if !out.contains(&n.to_string()) {
                        out.push(n.to_string());
                    }
                    if let Some((_, ts)) = pool.iter().find(|p| p.0 == *n) {
                        for &(slot, new) in &it.set {
                            let mut own: Vec<u64> = ts.iter().filter(|t| t.0 == slot).map(|t| t.1).collect();
                            own.sort_unstable();
                            own.dedup();
                            if let [old] = own[..] {
                                if old != new && !remap.iter().any(|r| r.0 == old) {
                                    remap.push((old, new));
                                }
                            }
                        }
                    }
                }
            }
            out
        }
        None => Vec::new(),
    };
    eprintln!(
        "[t6m:peds] {name}: wardrobe {} ({} seed textures, {:.0}s): {}",
        best.as_ref().map(|b| format!("{} ({} shared)", b.1, b.0)).unwrap_or_else(|| "none".into()),
        seed.len(),
        t.elapsed().as_secs_f32(),
        result.join(", ")
    );
    let _ = fs::create_dir_all(cache.parent().unwrap());
    let rm: Vec<[String; 2]> = remap.iter().map(|(a, b)| [format!("{a:016x}"), format!("{b:016x}")]).collect();
    let _ = fs::write(&cache, serde_json::json!({"v": CACHE_V, "resource": best.as_ref().map(|b| b.1.clone()), "parts": result, "remap": rm}).to_string());
    (!result.is_empty()).then_some((result, remap))
}
