//! Static collision: 6m physics bounds (sixng bounds decoder, already in
//! world space) merged into one t6m_col_XX_YY.ybn per 512 m cell.
//!
//! The bounds' world bboxes are cached in <work>/fivem_collision_index.jsonl
//! (one decode pass over every unnamed rageobj resource of levels.rpf).
//! GTA V material ids are not mapped yet: everything is material 0 (default).

use super::{sha, Ctx, TOOL};
use crate::gta5::ybn;
use rayon::prelude::*;
use serde_json::{json, Value};
use sixng_core::{
    resource::Resource,
    classify::Kind,
    scan::{ArchiveIndex, Entry},
};
use std::{collections::BTreeMap, error::Error, fs, io::Write};

const CELL: f32 = 512.0;

fn decode(path: &std::path::Path, e: &Entry) -> Option<sixng_core::bounds::Collision> {
    let r = Resource::load_sys(path, e).ok()?;
    let c = sixng_core::bounds::decode(&r)?;
    (!c.by_mat.values().all(|x| x.1.is_empty())).then_some(c)
}

pub fn build(ctx: &Ctx, index: &ArchiveIndex) -> Result<(), Box<dyn Error>> {
    let rpf = ctx.o.game.join("levels.rpf");
    let cands: Vec<&Entry> = index.entries.iter().filter(|e| e.kind == Kind::RageObj && e.name.is_none()).collect();
    // id -> world bbox of entries that hold collision
    let ip = ctx.o.work.join("fivem_collision_index.jsonl");
    let mut boxes: BTreeMap<String, [[f32; 3]; 2]> = BTreeMap::new();
    let cached = fs::read_to_string(&ip).ok().filter(|t| t.lines().next().is_some_and(|l| l.contains(&format!("\"entries\":{}", cands.len()))));
    if let Some(t) = cached {
        for l in t.lines().skip(1) {
            if let Ok(v) = serde_json::from_str::<Value>(l) {
                if let (Some(id), Ok(b)) = (v["id"].as_str(), serde_json::from_value::<[[f32; 3]; 2]>(v["bbox"].clone())) {
                    boxes.insert(id.to_string(), b);
                }
            }
        }
    } else {
        eprintln!("collision: indexing {} resources (once; cached in {})", cands.len(), ip.display());
        let found: Vec<(String, [[f32; 3]; 2])> = cands.par_iter().filter_map(|e| decode(&rpf, e).map(|c| (e.id.clone(), c.bbox))).collect();
        let mut f = fs::File::create(&ip)?;
        writeln!(f, "{}", json!({"tool": TOOL, "entries": cands.len()}))?;
        for (id, b) in &found {
            writeln!(f, "{}", json!({"id": id, "bbox": b}))?;
            boxes.insert(id.clone(), *b);
        }
    }
    let by_id: BTreeMap<&str, &Entry> = cands.iter().map(|e| (e.id.as_str(), *e)).collect();
    let sel: Vec<&Entry> = boxes.iter().filter(|(_, b)| ctx.o.in_bbox(b[0], b[1])).filter_map(|(id, _)| by_id.get(id.as_str()).copied()).collect();
    eprintln!("collision: {} bounds in region", sel.len());
    // cells by bbox centre
    let mut cells: BTreeMap<(i64, i64), Vec<&Entry>> = BTreeMap::new();
    for e in &sel {
        let b = boxes[&e.id];
        let c = [(b[0][0] + b[1][0]) / 2.0, (b[0][1] + b[1][1]) / 2.0];
        cells.entry(((c[0] / CELL).floor() as i64, (c[1] / CELL).floor() as i64)).or_default().push(e);
    }
    let res: Vec<Result<(), String>> = cells
        .par_iter()
        .map(|((cx, cy), es)| {
            let file = format!("t6m_col_{:02}_{:02}.ybn", cx + 50, cy + 50);
            let fp = sha(&format!("{TOOL}|col|{}", es.iter().map(|e| e.id.as_str()).collect::<Vec<_>>().join(",")));
            if let Some(info) = ctx.up_to_date(&file, &fp) {
                ctx.emit(&file, &fp, info, None)?;
                return Ok(());
            }
            let (mut verts, mut tris): (Vec<[f32; 3]>, Vec<[u32; 3]>) = (Vec::new(), Vec::new());
            for e in es {
                let Some(c) = decode(&rpf, e) else { continue };
                for (v, idx) in c.by_mat.values() {
                    let base = verts.len() as u32;
                    verts.extend_from_slice(v);
                    tris.extend(idx.chunks_exact(3).map(|t| [base + t[0], base + t[1], base + t[2]]));
                }
            }
            if tris.is_empty() {
                return Ok(());
            }
            let bytes = ybn::build_ybn(&verts, &tris, 0).map_err(|e| format!("{file}: {e}"))?;
            let info = json!({"bounds": es.len(), "triangles": tris.len()});
            let s = ctx.emit(&file, &fp, info, Some(bytes))?;
            ctx.report.lock().unwrap().converted.push(json!({"kind": "collision", "ybn": s, "bounds": es.len(), "triangles": tris.len()}));
            Ok(())
        })
        .collect();
    for r in res {
        r?;
    }
    Ok(())
}
