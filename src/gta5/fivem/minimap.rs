//! Minimap and zone names.
//!
//! - Radar: GTA V draws its land layer from six 1024 px DXT5 textures
//!   `minimap_sea_<row>_<col>` (3 rows x 2 columns, each in its own ytd);
//!   transparent pixels show the sea. Tile size 4500 m, top-left corner at
//!   (-4140, 8400): checked against the vanilla tiles with `fivem gta5` (land
//!   alpha vs vanilla collision positions, best fit 4.4-4.5 m/px, origin
//!   within one fit step). The terrain of the region (after --offset) is
//!   rasterised there as shaded relief; everything else is sea, since Los
//!   Santos is removed. One fingerprint over the terrain, bbox and offset.
//! - Zone names: each 512 m cell of the region is named after the most common
//!   6m archetype prefix placed there (`ma`, `nb`, `vb`, ...), or the name
//!   `<work>/zone_names.json` ({"prefix": "Name"}) gives it; adjacent cells
//!   with one name merge. `t6m_zones.lua` (client script) shows the name in
//!   place of GTA V's area name.

use super::geom::V3;
use super::Ctx;
use crate::gta5::ytd::{self, Tex, TexFormat};
use rayon::prelude::*;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};

const MINIMAP_V: &str = "minimap1";
const N: usize = 1024;
const TILE: f32 = 4500.0;
const X0: f32 = -4140.0;
const Y0: f32 = 8400.0;
const PX: f32 = TILE / N as f32;

/// Rasterise `tiles` (6m terrain, world space) into the six radar tiles.
pub fn build(ctx: &Ctx, tiles: &[(&[V3], &[[u32; 3]])]) -> Result<(), String> {
    let off = ctx.o.offset;
    let fp = {
        let h = tiles.iter().flat_map(|(v, _)| v.iter()).fold(0u64, |h, v| h.rotate_left(7) ^ (v[0].to_bits() as u64) << 20 ^ v[1].to_bits() as u64 ^ (v[2].to_bits() as u64) << 40);
        format!("{MINIMAP_V}|{h:x}|{}|{:?}|{:?}", tiles.len(), ctx.o.bbox, off)
    };
    let files: Vec<String> = (0..6).map(|i| format!("minimap_sea_{}_{}.ytd", i / 2, i % 2)).collect();
    if files.iter().all(|f| ctx.up_to_date(f, &fp).is_some()) {
        for f in &files {
            ctx.emit(f, &fp, json!({}), None)?;
        }
        return Ok(());
    }
    let (w, h) = (2 * N, 3 * N);
    let mut z = vec![f32::NAN; w * h];
    for (v, tris) in tiles {
        let p: Vec<[f32; 3]> = v.iter().map(|q| [(q[0] + off[0] - X0) / PX, (Y0 - q[1] - off[1]) / PX, q[2] + off[2]]).collect();
        for t in tris.iter() {
            let [a, b, c] = t.map(|i| p[i as usize]);
            let (x0, x1) = (a[0].min(b[0]).min(c[0]).floor().max(0.0) as i64, (a[0].max(b[0]).max(c[0]).ceil() as i64).min(w as i64 - 1));
            let (y0, y1) = (a[1].min(b[1]).min(c[1]).floor().max(0.0) as i64, (a[1].max(b[1]).max(c[1]).ceil() as i64).min(h as i64 - 1));
            let d = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
            if x0 > x1 || y0 > y1 || d.abs() < 1e-9 {
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (gx, gy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let l0 = ((b[1] - c[1]) * (gx - c[0]) + (c[0] - b[0]) * (gy - c[1])) / d;
                    let l1 = ((c[1] - a[1]) * (gx - c[0]) + (a[0] - c[0]) * (gy - c[1])) / d;
                    let l2 = 1.0 - l0 - l1;
                    if l0 >= -0.02 && l1 >= -0.02 && l2 >= -0.02 {
                        let i = y as usize * w + x as usize;
                        let zz = l0 * a[2] + l1 * b[2] + l2 * c[2];
                        if z[i].is_nan() || zz > z[i] {
                            z[i] = zz;
                        }
                    }
                }
            }
        }
    }
    // shaded relief: lowland green -> hills brown -> rock grey, lit from the north-west
    let at = |x: usize, y: usize| z[y.min(h - 1) * w + x.min(w - 1)];
    let rgba: Vec<u8> = (0..w * h)
        .into_par_iter()
        .flat_map_iter(|i| {
            let (x, y) = (i % w, i / w);
            let zz = z[i];
            if zz.is_nan() || zz < 0.3 {
                return [0u8; 4];
            }
            let zx = |xx: usize| { let v = at(xx, y); if v.is_nan() { zz } else { v } };
            let zy = |yy: usize| { let v = at(x, yy); if v.is_nan() { zz } else { v } };
            let (dx, dy) = ((zx(x + 1) - zx(x.saturating_sub(1))) / (2.0 * PX), (zy(y + 1) - zy(y.saturating_sub(1))) / (2.0 * PX));
            let shade = (1.0 - 0.9 * (dx + dy)).clamp(0.55, 1.25);
            let t = (zz / 300.0).clamp(0.0, 1.0);
            let base = if zz < 2.0 { [214.0, 204.0, 160.0] } else { [118.0 + 60.0 * t, 140.0 - 10.0 * t, 92.0 + 30.0 * t] };
            let c = |k: usize| (base[k] * shade).clamp(0.0, 255.0) as u8;
            [c(0), c(1), c(2), 255]
        })
        .collect();
    for (ti, f) in files.iter().enumerate() {
        let (r, c) = (ti / 2, ti % 2);
        let mut px = vec![0u8; N * N * 4];
        for y in 0..N {
            let s = ((r * N + y) * w + c * N) * 4;
            px[y * N * 4..(y + 1) * N * 4].copy_from_slice(&rgba[s..s + N * 4]);
        }
        let mut data = vec![0u8; texpresso::Format::Bc3.compressed_size(N, N)];
        texpresso::Format::Bc3.compress(&px, N, N, texpresso::Params::default(), &mut data);
        let name = f.trim_end_matches(".ytd");
        let tex = Tex { name: name.into(), width: N as u16, height: N as u16, levels: 1, format: TexFormat::DXT5, data, unk32: 0, usage_data: 0, extra_flags: 0 };
        let b = ytd::build_ytd(&[tex])?;
        ctx.emit(f, &fp, json!({"tile": [r, c]}), Some(b))?;
    }
    let land = rgba.chunks_exact(4).filter(|p| p[3] != 0).count();
    eprintln!("minimap: 6 radar tiles, {land} land pixels ({:.1} km2)", land as f32 * PX * PX / 1e6);
    Ok(())
}

/// Zone names from the building layout (see module docs); writes
/// `<out>/t6m_zones.lua` (compared byte for byte).
pub fn zones(ctx: &Ctx) -> Result<(), String> {
    let names: HashMap<String, String> = std::fs::read(ctx.o.work.join("zone_names.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let layout = ctx.layout.lock().unwrap();
    let mut cells: BTreeMap<(i64, i64), HashMap<String, usize>> = BTreeMap::new();
    for (ym, arcs) in &layout.ymaps {
        let cell = if let Some(r) = ym.strip_prefix("t6m_bn_") {
            r.split_once('_').and_then(|(a, b)| Some(((a.parse::<i64>().ok()? - 200).div_euclid(4), (b.parse::<i64>().ok()? - 200).div_euclid(4))))
        } else if let Some(r) = ym.strip_prefix("t6m_bld_") {
            r.split_once('_').and_then(|(a, b)| Some((a.parse::<i64>().ok()? - 50, b.parse::<i64>().ok()? - 50)))
        } else {
            None
        };
        let Some(cell) = cell else { continue };
        let e = cells.entry(cell).or_default();
        for a in arcs {
            let a = a.strip_prefix("r6m_").unwrap_or(a);
            let p = a.split('_').next().unwrap_or("");
            if p.len() >= 2 && p.len() <= 4 && p.chars().all(|c| c.is_ascii_alphabetic()) && !["prop", "lod", "slod", "int"].contains(&p) {
                *e.entry(p.to_string()).or_default() += 1;
            }
        }
    }
    drop(layout);
    let off = ctx.o.offset;
    let mut rows: Vec<(i64, i64, i64, String)> = Vec::new(); // (cy, cx0, cx1, name)
    for ((cx, cy), m) in &cells {
        let Some((p, _)) = m.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))) else { continue };
        let name = names.get(p).cloned().unwrap_or_else(|| p.to_uppercase());
        match rows.last_mut() {
            Some(r) if r.0 == *cy && r.2 + 1 == *cx && r.3 == name => r.2 = *cx,
            _ => rows.push((*cy, *cx, *cx, name)),
        }
    }
    let mut t = String::from("-- generated by 6ixpack fivem build: 6m zone names (512 m cells)\nlocal Z = {\n");
    for (cy, a, b, n) in &rows {
        let (x0, x1) = (*a as f32 * 512.0 + off[0], (*b + 1) as f32 * 512.0 + off[0]);
        let (y0, y1) = (*cy as f32 * 512.0 + off[1], (*cy + 1) as f32 * 512.0 + off[1]);
        t += &format!("  {{{x0:.0}, {y0:.0}, {x1:.0}, {y1:.0}, {:?}}},\n", n);
    }
    t += r#"}
local function zoneAt(x, y)
  for _, z in ipairs(Z) do
    if x >= z[1] and y >= z[2] and x < z[3] and y < z[4] then return z[5] end
  end
end
CreateThread(function()
  while true do
    local p = GetEntityCoords(PlayerPedId())
    local n = zoneAt(p.x, p.y)
    if n then
      HideHudComponentThisFrame(7) -- vanilla area name
      SetTextFont(4); SetTextScale(0.0, 0.5); SetTextOutline()
      BeginTextCommandDisplayText("STRING"); AddTextComponentSubstringPlayerName(n)
      EndTextCommandDisplayText(0.17, 0.93)
      Wait(0)
    else
      Wait(500)
    end
  end
end)
"#;
    let p = ctx.o.out.join("t6m_zones.lua");
    if rows.is_empty() {
        let _ = std::fs::remove_file(&p);
        return Ok(());
    }
    let same = std::fs::read_to_string(&p).is_ok_and(|x| x == t);
    if !same {
        std::fs::write(&p, &t).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    eprintln!("zones: {} named strips over {} cells", rows.len(), cells.len());
    ctx.report.lock().unwrap().outputs.push(json!({"file": "t6m_zones.lua", "status": if same {"up-to-date"} else {"written"}, "zones": rows.len()}));
    Ok(())
}
