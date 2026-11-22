//! GTA V path nodes (`nodes<area>.ynd`, resource version 1): the road graph
//! AI traffic drives along, one file per 512 m cell of the 32 x 32 grid
//! from (-8192, -8192).
//!
//! Layout from CodeWalker.Core `Resources/Node.cs` (`NodeDictionary` 0x70,
//! `Node` 40 bytes, `NodeLink` 8 bytes) and the flag accessors in
//! `FileTypes/YndFile.cs`; cross-checked with rage-formats `ynd.rs`.

use super::rsc::{Builder, Seg, W};

pub const CELL: f32 = 512.0;
pub const ORIGIN: f32 = -8192.0;
const VFT: u32 = 0x4061_E3B0;

/// The area id (`y * 32 + x`) holding a world position; None off the grid.
pub fn area_of(x: f32, y: f32) -> Option<u16> {
    let cx = ((x - ORIGIN) / CELL).floor();
    let cy = ((y - ORIGIN) / CELL).floor();
    ((0.0..32.0).contains(&cx) && (0.0..32.0).contains(&cy)).then(|| (cy as u16) * 32 + cx as u16)
}

/// World bbox (xy) of an area.
pub fn area_bounds(area: u16) -> [f32; 4] {
    let (cx, cy) = ((area % 32) as f32, (area / 32) as f32);
    [ORIGIN + cx * CELL, ORIGIN + cy * CELL, ORIGIN + (cx + 1.0) * CELL, ORIGIN + (cy + 1.0) * CELL]
}

#[derive(Clone, Debug, Default, Hash)]
pub struct Link {
    pub area: u16,
    pub node: u16,
    pub flags0: u8,
    pub flags1: u8,
    /// bits 5-7 lanes forward, 2-4 lanes backward, bit 1 shortcut, bit 0 no-nav
    pub flags2: u8,
    pub length: u8,
}

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub pos: [f32; 3],
    pub street: u32,
    /// Flags0..Flags4 (see `YndNode`): f0 bit 3 off-road, bit 5 no big
    /// vehicles; f1 bit 0 slip road, bits 3-7 special type; f2 bit 0 no gps,
    /// bit 2 junction, bit 6 highway, bits 4/7 switched off; f3 bit 0
    /// tunnel; f4 bits 0-3 density.
    pub flags: [u8; 5],
    /// 0 slow .. 3 faster (bits 1-2 of LinkCountFlags).
    pub speed: u8,
    pub links: Vec<Link>,
}

/// One area's file. Node ids are the slice order (vehicle nodes; no ped
/// nodes are written), and every link must already name its target by
/// (area, index in that area's slice).
pub fn build_ynd(area: u16, nodes: &[Node]) -> Result<Vec<u8>, String> {
    if nodes.len() > u16::MAX as usize {
        return Err(format!("area {area}: {} nodes", nodes.len()));
    }
    let q = |v: f32, s: f32| -> Result<i16, String> {
        let r = (v * s).round();
        if (i16::MIN as f32..=i16::MAX as f32).contains(&r) { Ok(r as i16) } else { Err(format!("area {area}: coordinate {v} out of range")) }
    };
    let mut b = Builder::new();
    let root = b.reserve(Seg::Sys);
    let mut nw = W::new();
    let mut lw = W::new();
    let mut nlinks = 0usize;
    for (i, n) in nodes.iter().enumerate() {
        if n.links.len() > 31 {
            return Err(format!("area {area}: node {i} has {} links", n.links.len()));
        }
        nw.zeros(16).u16(area).u16(i as u16).u32(n.street).u16(0).u16(nlinks as u16);
        nw.u16(q(n.pos[0], 4.0)? as u16).u16(q(n.pos[1], 4.0)? as u16).u8(n.flags[0]).u8(n.flags[1]).u16(q(n.pos[2], 32.0)? as u16);
        nw.u8(n.flags[2]).u8(((n.links.len() as u8) << 3) | ((n.speed & 3) << 1)).u8(n.flags[3]).u8(n.flags[4]);
        for l in &n.links {
            lw.u16(l.area).u16(l.node).u8(l.flags0).u8(l.flags1).u8(l.flags2).u8(l.length);
        }
        nlinks += n.links.len();
        if nlinks > u16::MAX as usize {
            return Err(format!("area {area}: more than 65535 links"));
        }
    }
    let nb = (!nodes.is_empty()).then(|| b.add(Seg::Sys, nw));
    let lb = (nlinks > 0).then(|| b.add(Seg::Sys, lw));
    let mut r = W::new();
    r.u32(VFT).u32(1).u64(0);
    r.ptr(nb).u32(nodes.len() as u32).u32(nodes.len() as u32).u32(0).u32(0);
    r.ptr(lb).u32(nlinks as u32).u32(0);
    r.u64(0).u64(0).u32(1).u32(0); // junctions, heightmap bytes, Unk48 = 1
    r.u64(0).u16(0).u16(0).u32(0); // junction refs
    r.u32(0).u32(0).u32(0).u32(0);
    debug_assert_eq!(r.b.len(), 0x70);
    b.set(root, r);
    b.build(1)
}
