// Research probe for BCPack (not part of the library).
use sixng_core::ttpf::*;
use std::collections::BTreeMap;
use std::path::Path;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let set = TtpfSet::open_dir(Path::new(&a[1]));
    match a[2].as_str() {
        "stats" => {
            let mut st: BTreeMap<u8, (u64, u64, u64)> = BTreeMap::new();
            let mut fm: BTreeMap<(u8, u32), u64> = BTreeMap::new();
            for s in &set.stores {
                for w in s.chunks.windows(2) {
                    let c = w[0].2 & 0x3F;
                    let e = st.entry(c).or_default();
                    e.0 += 1; e.1 += w[1].1 - w[0].1; e.2 += w[1].0 - w[0].0;
                }
                for t in &s.textures {
                    for w in s.chunks.windows(2) {
                        if w[1].1 > t.virt && w[0].1 < t.virt + t.size && w[0].2 & 0x1C != 0 {
                            *fm.entry((w[0].2 & 0x3F, t.dxgi_format)).or_default() += 1; break;
                        }
                    }
                }
            }
            for (c, v) in st { println!("codec {c:#04x}: {} chunks, virt {} MB, comp {} MB", v.0, v.1 >> 20, v.2 >> 20); }
            for (k, v) in fm { println!("codec {:#04x} dxgi {}: {v} textures", k.0, k.1); }
        }
        "full" => {
            let mut st: BTreeMap<u8, [usize; 5]> = BTreeMap::new();
            let only = a.get(3).and_then(|x| u8::from_str_radix(x, 16).ok());
            let stride: usize = a.get(4).and_then(|x| x.parse().ok()).unwrap_or(1);
            let mut seen = 0usize;
            for s in &set.stores { for (i, w) in s.chunks.windows(2).enumerate() {
                let c = w[0].2 & 0x3F;
                if w[0].2 & 0x1C == 0 || !sixng_core::bcpack::supported(c) { continue; }
                if only.is_some_and(|o| o != c) { continue; }
                seen += 1;
                if seen % stride != 0 { continue; }
                let comp = chunk_comp(s, i);
                let e = st.entry(c).or_default();
                e[0] += 1;
                match sixng_core::bcpack::decode(c, &comp, (w[1].1 - w[0].1) as usize) {
                    Ok(d) => { e[2] += d.data.len() / if c == 0x10 { 8 } else { 16 }; e[3] += d.unknown; e[4] += d.inconsistent; }
                    Err(x) => { e[1] += 1; if e[1] <= 2 { println!("{c:#x} {} {i}: {x}", s.label()); } }
                }
            } }
            for (c, v) in st { println!("codec {c:#04x}: chunks {} failed {} blocks {} unknown-hist {} inconsistent {}", v[0], v[1], v[2], v[3], v[4]); }
        }
        "calib" => {
            let t0 = std::time::Instant::now();
            let h = sixng_core::bcpack_calib::calibrate(&set);
            std::fs::write(&a[3], h.to_bytes()).unwrap();
            let ep = h.bc7_ep_bits.iter().flatten().filter(|&&b| b > 0).count();
            let ix = h.bc7_ix_bits.iter().flatten().filter(|&&b| b > 0).count();
            println!("{:.1}s bc4 {} bc7 ep slots {ep} ix slots {ix} bc6h ep {} ix {}", t0.elapsed().as_secs_f64(), h.bc4.iter().flatten().count(), h.bc6h_ecls.iter().filter(|&&c| c > 0).count(), h.bc6h_icls.iter().filter(|&&c| c > 0).count());
        }
        "refine" => {
            let mut h = sixng_core::bcpack::History::from_bytes(&std::fs::read(&a[3]).unwrap()).unwrap();
            let t0 = std::time::Instant::now();
            sixng_core::bcpack_refine::refine_indices(&set, &mut h);
            sixng_core::bcpack_refine::refine_bc7_pbits(&set, &mut h);
            std::fs::write(&a[4], h.to_bytes()).unwrap();
            println!("refined in {:.1}s", t0.elapsed().as_secs_f64());
        }
        "mipcheck" => {
            // mipcheck <codec hex> <max textures>: BCPack mip m vs plain mip m+1, per block
            let codec = u8::from_str_radix(&a[3], 16).unwrap();
            let lim: usize = a[4].parse().unwrap();
            let mut agg: BTreeMap<String, (usize, f64, usize)> = BTreeMap::new();
            let mut ntex = 0;
            'o: for s in &set.stores { for t in &s.textures {
                if t.slices() != 1 || (9..=12).contains(&t.tile_mode()) || t.split_2b { continue; }
                let Some(l) = t.layout() else { continue };
                let nm = l.mips.len();
                let ck = |m: usize| -> Vec<usize> { let (x, y) = l.mip_span(m); (0..s.chunks.len() - 1).filter(|&i| s.chunks[i + 1].1 > t.virt + x && s.chunks[i].1 < t.virt + y).collect() };
                let Some(m) = (0..nm.saturating_sub(1)).find(|&m| ck(m).iter().any(|&i| s.chunks[i].2 & 0x3F == codec && s.chunks[i].2 & 0x1C != 0) && ck(m + 1).iter().all(|&i| s.chunks[i].2 & 0x1C == 0)) else { continue };
                let Ok((data, _)) = s.data_ex(t) else { continue };
                let (Ok((a0, w0, h0)), Ok((a1, w1, _))) = (decode_rgba(t, &data, m as u32), decode_rgba(t, &data, m as u32 + 1)) else { continue };
                let unq = codec == 0x18 && std::env::var("UNQ").is_ok();
                let (u0, u1) = if unq {
                    let f = |mm: usize| { let p = &l.mips[mm]; sixng_core::bcpack::bc6h_unq_image(&l.untile(&data, mm), p.w * 4, p.h * 4, false).iter().map(|&v| ((v + 1.0).log2() * 16.0).min(255.0) as u8).collect::<Vec<u8>>() };
                    (f(m), f(m + 1))
                } else { (a0.clone(), a1.clone()) };
                let (a0, a1) = (u0, u1);
                ntex += 1;
                let mut cache: BTreeMap<usize, sixng_core::bcpack::Decoded> = BTreeMap::new();
                for by in 0..(h0 / 4) { for bx in 0..(w0 / 4) {
                    let mut e = 0.0;
                    for c in 0..3 {
                        let mut s0 = 0.0; for y in 0..4 { for x in 0..4 { s0 += a0[(((by * 4 + y) * w0 + bx * 4 + x) * 4 + c) as usize] as f64; } }
                        let mut s1 = 0.0; for y in 0..2 { for x in 0..2 { s1 += a1[(((by * 2 + y) * w1 + bx * 2 + x) * 4 + c) as usize] as f64; } }
                        e += (s0 / 16.0 - s1 / 4.0).abs();
                    }
                    let v = t.virt + l.addr(m, 0, bx, by);
                    let Some(ci) = (0..s.chunks.len() - 1).find(|&i| s.chunks[i].1 <= v && s.chunks[i + 1].1 > v) else { continue };
                    if s.chunks[ci].2 & 0x1C == 0 { continue; }
                    let d = cache.entry(ci).or_insert_with(|| sixng_core::bcpack::decode(codec, &chunk_comp(s, ci), (s.chunks[ci + 1].1 - s.chunks[ci].1) as usize).unwrap());
                    let bi = ((v - s.chunks[ci].1) / 16) as usize;
                    let key = match d.info.get(bi) { Some(&x) => { let de = (x >> 8) & 255; let kind = if de == 0 { "none".to_string() } else if (x >> 16) & 1 != 0 { "hist".into() } else { format!("{}ref{}", if (x >> 17) & 1 != 0 { "H" } else { "N" }, x >> 20) }; format!("m{:02} {kind}", x & 255) } None => "all".into() };
                    let g = agg.entry(key).or_default(); g.0 += 1; g.1 += e; if e > 60.0 { g.2 += 1; }
                } }
                if ntex >= lim { break 'o; }
            } }
            println!("{ntex} textures");
            for (k, v) in agg { if v.0 >= 50 { println!("{k}: blocks {} mean err {:.1} bad {:.1}%", v.0, v.1 / v.0 as f64, 100.0 * v.2 as f64 / v.0 as f64); } }
        }
        "dump" => {
            // dump <store label substr> <chunk> <outdir>
            let s = set.stores.iter().find(|s| s.label().contains(a[3].as_str())).unwrap();
            let i: usize = a[4].parse().unwrap();
            let comp = chunk_comp(s, i);
            let c = s.chunks[i].2 & 0x3F;
            let vlen = (s.chunks[i + 1].1 - s.chunks[i].1) as usize;
            let d = sixng_core::bcpack::decode(c, &comp, vlen).unwrap();
            std::fs::write(format!("{}/comp.bin", a[5]), &comp).unwrap();
            std::fs::write(format!("{}/rust.bin", a[5]), &d.data).unwrap();
            println!("codec {c} vlen {vlen} unknown {} inconsistent {}", d.unknown, d.inconsistent);
        }
        "lanes" => {
            // lanes <codec> <widths comma> <target num/den> <count>
            let codec = u8::from_str_radix(&a[3], 16).unwrap();
            let ab: Vec<u8> = a[4].split(',').map(|x| x.parse::<u8>().unwrap() - 1).collect();
            let (num, den): (usize, usize) = { let v: Vec<usize> = a[5].split('/').map(|x| x.parse().unwrap()).collect(); (v[0], v[1]) };
            let cnt: usize = a[6].parse().unwrap();
            let (mut ok, mut tot) = (0, 0);
            'o: for s in &set.stores {
                for (i, w) in s.chunks.windows(2).enumerate() {
                    if w[0].2 & 0x3F != codec { continue; }
                    let comp = chunk_comp(s, i);
                    let vlen = (w[1].1 - w[0].1) as usize;
                    let tgt = vlen / 16 * num / den;
                    let r = bcpack::rans::decode_slice(&comp, &ab, tgt);
                    tot += 1;
                    match r { Ok(_) => ok += 1, Err(e) => if tot <= 3 { println!("{} {i} vlen {vlen}: {e}", s.label()) } }
                    if tot >= cnt { break 'o; }
                }
            }
            println!("{ok}/{tot}");
        }
        "dups" => {
            // textures touching BCPack (non-BC1) whose hash also exists in a store with mip 0 clean
            let mut byhash: std::collections::HashMap<u64, Vec<(usize, usize)>> = Default::default();
            for (si, s) in set.stores.iter().enumerate() { for (ti, t) in s.textures.iter().enumerate() { byhash.entry(t.hash).or_default().push((si, ti)); } }
            let mut n: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
            for v in byhash.values() {
                let bad: Vec<_> = v.iter().filter(|&&(si, ti)| { let s = &set.stores[si]; let t = &s.textures[ti]; s.bcpack_bytes(t.virt, t.size) > 0 }).collect();
                if bad.is_empty() { continue; }
                let t0 = &set.stores[bad[0].0].textures[bad[0].1];
                let e = n.entry(t0.dxgi_format).or_default();
                e.0 += 1;
                let good = v.iter().find(|&&(si, ti)| { let s = &set.stores[si]; let t = &s.textures[ti]; s.bcpack_bytes(t.virt, t.size) == 0 && t.d0 == t0.d0 });
                if let Some(&(si, ti)) = good { e.1 += 1; if e.1 <= 3 { println!("dxgi {} {:016x} bad {} good {} {}x{}", t0.dxgi_format, t0.hash, set.stores[bad[0].0].label(), set.stores[si].label(), t0.width, t0.height); } let _ = ti; }
            }
            for (k, v) in n { println!("dxgi {k}: {} hashes, {} with clean twin", v.0, v.1); }
        }
        "syms" => {
            // syms <store substr> <chunk idx> <widths> -> writes raw u16 lane-major symbols to stdout file
            let s = set.stores.iter().find(|s| s.label().contains(&a[3])).unwrap();
            let i: usize = a[4].parse().unwrap();
            let ab: Vec<u8> = a[5].split(',').map(|x| x.parse::<u8>().unwrap() - 1).collect();
            let vlen = (s.chunks[i + 1].1 - s.chunks[i].1) as usize;
            let comp = chunk_comp(s, i);
            let mul: usize = a[6].parse().unwrap();
            let r = bcpack::rans::decode_slice(&comp, &ab, vlen / 16 * mul).unwrap();
            let mut out = Vec::new();
            for v in &r.symbols { out.extend_from_slice(&v.to_le_bytes()); }
            std::fs::write(&a[7], out).unwrap();
            println!("lanes {} target {} codec {:#x}", r.lanes, r.target, s.chunks[i].2);
        }
        "list" => {
            // list <codec>: chunks with textures overlapping
            let codec = u8::from_str_radix(&a[3], 16).unwrap();
            for s in &set.stores { for (i, w) in s.chunks.windows(2).enumerate() { if w[0].2 & 0x3F != codec { continue; }
                let ts: Vec<String> = s.textures.iter().filter(|t| t.virt < w[1].1 && t.virt + t.size > w[0].1).map(|t| format!("{:016x}:{}x{}:m{}:@{}", t.hash, t.width, t.height, t.mips(), t.virt as i64 - w[0].1 as i64)).collect();
                println!("{} {i} vlen {} {}", s.label(), w[1].1 - w[0].1, ts.join(" "));
            } }
        }
        "export" => {
            // export <codec> <count> <outdir> <maxblocks> <lanewidths> <mul>
            let codec = u8::from_str_radix(&a[3], 16).unwrap();
            let cnt: usize = a[4].parse().unwrap();
            let od = Path::new(&a[5]); std::fs::create_dir_all(od).unwrap();
            let maxb: usize = a[6].parse().unwrap();
            let ab: Vec<u8> = a[7].split(',').map(|x| x.parse::<u8>().unwrap() - 1).collect();
            let mul: usize = a[8].parse().unwrap();
            let skip: usize = a.get(9).map(|x| x.parse().unwrap()).unwrap_or(1);
            let mut n = 0; let mut seen = 0;
            'o: for s in &set.stores { for (ci, w) in s.chunks.windows(2).enumerate() {
                if w[0].2 & 0x3F != codec { continue; }
                let (v0, v1) = (w[0].1, w[1].1);
                let fmt = sixng_core::texture::Fmt::from_dxgi(0);
                let _ = fmt;
                let Some(t) = s.textures.iter().find(|t| t.virt < v1 && t.virt + t.size > v0) else { continue };
                let Some(l) = t.layout() else { continue };
                if l.slices != 1 { continue; }
                let bpe = l.bpe as u64;
                let nb = ((v1 - v0) / bpe) as usize;
                if nb > maxb { continue; }
                // which mip does this chunk hold?
                let Some(m) = (0..l.mips.len()).find(|&m| { let (a0, b0) = l.mip_span(m); t.virt + a0 < v1 && t.virt + b0 > v0 }) else { continue };
                if m + 1 >= l.mips.len() { continue; }
                let (a1, b1) = l.mip_span(m + 1);
                if s.bcpack_bytes(t.virt + a1, b1 - a1) > 0 { continue; }
                seen += 1; if seen % skip != 0 { continue; }
                let comp = chunk_comp(s, ci);
                let Ok(r) = bcpack::rans::decode_slice(&comp, &ab, nb * mul) else { println!("fail {} {ci}", s.label()); continue };
                let (mw, mh) = (l.mips[m].w, l.mips[m].h);
                let mut map = vec![-1i32; nb];
                for y in 0..mh { for x in 0..mw { let ad = t.virt + l.addr(m, 0, x, y); if ad >= v0 && ad < v1 { map[((ad - v0) / bpe) as usize] = (y * mw + x) as i32; } } }
                let data = s.data(t).unwrap();
                let (rgba, w1, h1) = decode_rgba(t, &data, (m + 1) as u32).unwrap();
                let name = format!("{}_{ci}", s.label().replace(['/', '.'], "_"));
                let mut sb = Vec::new(); for v in &r.symbols { sb.extend_from_slice(&v.to_le_bytes()); }
                std::fs::write(od.join(format!("{name}.syms")), sb).unwrap();
                let mb: Vec<u8> = map.iter().flat_map(|v| v.to_le_bytes()).collect();
                std::fs::write(od.join(format!("{name}.map")), mb).unwrap();
                std::fs::write(od.join(format!("{name}.ref")), &rgba).unwrap();
                std::fs::write(od.join(format!("{name}.json")), format!("{{\"blocks\":{nb},\"lanes\":{},\"mul\":{mul},\"bw\":{mw},\"bh\":{mh},\"rw\":{w1},\"rh\":{h1},\"dxgi\":{},\"hash\":\"{:016x}\",\"mip\":{m},\"w\":{},\"h\":{}}}", ab.len(), t.dxgi_format, t.hash, t.width, t.height)).unwrap();
                n += 1; if n >= cnt { break 'o; }
            } }
            println!("exported {n}");
        }
        _ => {}
    }
}

pub fn chunk_comp(s: &Ttpf, i: usize) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let (foff, _, _, pad) = s.chunks[i];
    let fnext = s.chunks[i + 1].0;
    let mut f = std::fs::File::open(&s.path).unwrap();
    f.seek(SeekFrom::Start(foff)).unwrap();
    let mut v = vec![0u8; (fnext - foff - pad) as usize];
    f.read_exact(&mut v).unwrap();
    v
}
