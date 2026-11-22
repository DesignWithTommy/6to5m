//! Road / path-node graph (GTA V `nodes*.ynd` equivalent), stored as two
//! self-describing meta resources in `levels.rpf`:
//!
//! ```text
//! node file: 0c69a100 (60 B) x 63,808
//!   +0x00 a2a107f9 node (52 B): +0x00 vec3 position, +0x0C u64 flags,
//!         +0x14 06267d2f own address {u16 area, u16 index}, +0x18 / +0x1C
//!         two more addresses, +0x20 u32 street-name hash, +0x24 f16 speed
//!         limit (m/s: 4.47 / 13.4 / 17.9 / 22.35 = 10 / 30 / 40 / 50 mph),
//!         +0x26 i16, +0x28 u16, +0x2A..+0x30 u8 x6
//!   +0x34 address, +0x38 u32 index of the node's first link
//! link file: 0f2da4c2 (20 B) x 145,518
//!   +0x00 target address {u16 area, u16 index}, +0x04 u16 length (m),
//!   +0x06 u8 lanes to target, +0x07 u8 lanes from target, +0x08 u8,
//!   +0x0A u16, +0x0C u32 flags, +0x10 u16
//! ```
//! A node's links are `links[first .. next node's first]`.
use crate::meta7a::Meta;
use crate::resource::Resource;
use serde::Serialize;

pub const NODE: u32 = 0x0c69_a100;
pub const LINK: u32 = 0x0f2d_a4c2;
/// Junction list entries (no GTA V `.ynd` equivalent; own file).
pub const JUNCTION: u32 = 0xeba8_aebb;

/// Node flag names (bitset `Flags` at +0x0C, enum `0b354003`, 51 bits).
/// Recovered from the enum's own value hashes (case-sensitive joaat of
/// `PNF_<CamelCase>`); `None` = hash not recovered.
pub const NODE_FLAGS: [Option<&str>; 51] = [
    Some("NoGps"), Some("SlipJunction"), Some("SplitJunction"), Some("WaterNode"), Some("Junction"), Some("InTunnel"), Some("GpsOnly"), Some("SlipLane"),
    None, Some("LowBridge"), Some("River"), None, Some("NoThroughGps"), Some("RiverCrossing"), Some("Bridge"), None,
    None, Some("HighwaySlipRoad"), Some("IsInInterior"), Some("TrainStation"), None, Some("BarrierAhead"), Some("HighwayExitRoad"), Some("HasBikeLane"),
    Some("IsRaceTrack"), Some("AvoidGps"), Some("NoBigVehicles"), Some("SmallWorkVehicles"), Some("GiveWay"), Some("Stop"), Some("Yield"), None,
    Some("Crossing"), None, None, None, None, None, Some("AirportTaxi"), Some("AirportRunway"),
    None, Some("Elevated"), Some("RoadCrossing"), None, None, None, None, None,
    None, None, Some("UndergroundCarPark"),
];

/// Link flags (enum `c2804a82`, 11 bits). Bits 0, 2, 3, 4 are the game's
/// names (case-sensitive joaat of `PNLF_<Name>`); bits 5 and 10 are
/// inferred from geometry (`[...]`); the rest are unnamed.
pub const LINK_FLAGS: [&str; 11] = [
    "GpsCanGoBothWays", "bit1", "ShortCut", "LeadsToDeadEnd", "LeadsFromDeadEnd",
    "[unpaved/track]", "[no ambient traffic?]", "bit7", "bit8", "bit9", "[two-way road]",
];

pub fn link_flag_names(f: u32) -> Vec<String> {
    (0..11).filter(|b| f >> b & 1 == 1).map(|b| LINK_FLAGS[b].to_string()).collect()
}

pub fn node_flag_names(f: u64) -> Vec<String> {
    (0..51).filter(|b| f >> b & 1 == 1).map(|b| NODE_FLAGS[b].map(|s| s.to_string()).unwrap_or_else(|| format!("bit{b}"))).collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    /// RAGE world space, Z-up, metres.
    pub p: [f32; 3],
    /// {area, index} address the links refer to.
    pub address: [u16; 2],
    /// Speed limit, m/s (half float at +0x24).
    pub speed: f32,
    /// Street-name hash (0 = none; names not resolved).
    pub street: String,
    pub flags: String,
    /// Named node flags (`PNF_` names without the prefix; `bitN` = unnamed).
    pub flag_names: Vec<String>,
    /// Spatial {area, index} key at +0x18; junctions refer to nodes by it.
    pub key: [u16; 2],
    /// Enum at +0x2A (`0fe27c12`, 8 values, value 0 named "Default"): 1 on most roads.
    pub kind: u8,
    /// Enum at +0x2B (`391d8bab`, 14 values), a road class by its speeds:
    /// 0 freeway (60-70 mph), 1-2 highway / major road (40-60), 5 15-mph
    /// streets, 7 / 10 residential and car park (10 mph), 9 airport.
    pub road_class: u8,
    /// `NumLinks` (+0x2C) and `Density` (+0x2D, 0-15).
    pub num_links: u8,
    pub density: u8,
    /// u8 x6 at +0x2A (raw).
    pub bytes: String,
    #[serde(skip)]
    pub first_link: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Link {
    pub from: usize,
    pub to: usize,
    pub length: u16,
    pub lanes_to: u8,
    pub lanes_from: u8,
    /// `Flags` bitset at +0x0C (enum `c2804a82`, 11 bits). See `LINK_FLAGS`.
    pub flags: u32,
    /// Flag names: game names (`PNLF_` prefix dropped) where recovered,
    /// otherwise an inferred label in brackets or `bitN`.
    pub flag_names: Vec<String>,
    /// Raw 20 bytes for the undecoded fields.
    pub raw: String,
}

fn f16(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1F) as i32;
    let m = (h & 0x3FF) as f32;
    s * match e {
        0 => m / 1024.0 * 2f32.powi(-14),
        31 => f32::INFINITY,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn nodes(r: &Resource) -> Vec<Node> {
    let Some(m) = Meta::parse(r) else { return vec![] };
    m.objects(NODE)
        .into_iter()
        .map(|o| Node {
            p: [r.f32(o), r.f32(o + 4), r.f32(o + 8)],
            address: [r.u16(o + 0x14), r.u16(o + 0x16)],
            speed: f16(r.u16(o + 0x24)),
            street: format!("{:08x}", r.u32(o + 0x20)),
            flags: format!("{:016x}", r.u64(o + 0x0C)),
            flag_names: node_flag_names(r.u64(o + 0x0C)),
            key: [r.u16(o + 0x18), r.u16(o + 0x1A)],
            kind: r.sys[o + 0x2A],
            road_class: r.sys[o + 0x2B],
            num_links: r.sys[o + 0x2C],
            density: r.sys[o + 0x2D],
            bytes: hex(&r.sys[o + 0x2A..o + 0x30]),
            first_link: r.u32(o + 0x38),
        })
        .collect()
}

/// Raw link records (20 bytes each).
pub fn link_records(r: &Resource) -> Vec<Vec<u8>> {
    let Some(m) = Meta::parse(r) else { return vec![] };
    m.objects(LINK).into_iter().map(|o| r.sys[o..o + 20].to_vec()).collect()
}

/// Resolve links: node i owns records[first_link(i) .. first_link(i + 1)].
pub fn links(nodes: &[Node], recs: &[Vec<u8>]) -> (Vec<Link>, usize) {
    let by: std::collections::HashMap<[u16; 2], usize> = nodes.iter().enumerate().map(|(i, n)| (n.address, i)).collect();
    let mut out = Vec::with_capacity(recs.len());
    let mut bad = 0;
    for (i, n) in nodes.iter().enumerate() {
        let s = n.first_link as usize;
        let e = nodes.get(i + 1).map(|x| x.first_link as usize).unwrap_or(recs.len());
        for rec in recs.get(s..e.min(recs.len())).unwrap_or(&[]) {
            let a = [u16::from_le_bytes([rec[0], rec[1]]), u16::from_le_bytes([rec[2], rec[3]])];
            match by.get(&a) {
                Some(&to) => out.push(Link { from: i, to, length: u16::from_le_bytes([rec[4], rec[5]]), lanes_to: rec[6], lanes_from: rec[7], flags: u32::from_le_bytes([rec[12], rec[13], rec[14], rec[15]]), flag_names: link_flag_names(u32::from_le_bytes([rec[12], rec[13], rec[14], rec[15]])), raw: hex(rec) }),
                None => bad += 1,
            }
        }
    }
    (out, bad)
}

#[derive(Debug, Clone, Serialize)]
pub struct JunctionPath {
    /// Node index (into `nodes`) the turn path leads to.
    pub to: Option<usize>,
    /// vec3 at +0x90 / +0x9C: start and end of the path through the junction.
    pub start: [f32; 3],
    pub end: [f32; 3],
    /// `fPathLength` (+0x8C).
    pub length: f32,
    pub lane: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entrance {
    /// Entrance node index (median 16 m from the junction node).
    pub node: Option<usize>,
    pub lane: u8,
    /// `bIsStop` / `bIsYield` (+0x47 / +0x48): almost never set; stop and
    /// give-way are node flags (`Stop`, `Yield`).
    pub stop: bool,
    #[serde(rename = "yield")]
    pub yield_: bool,
    pub paths: Vec<JunctionPath>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Phase {
    /// `fDuration`, seconds.
    pub duration: f32,
    /// Lights in this phase: (u64 hash at +0, `boxId` at +8).
    pub lights: Vec<(String, u8)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Junction {
    pub node: Option<usize>,
    pub entrances: Vec<Entrance>,
    /// Traffic-light phases (empty = unsignalised junction).
    pub phases: Vec<Phase>,
    /// i32 at +0x38 (-1 on most unsignalised junctions).
    pub u38: i32,
}

/// `t5c` array: {u32 count, u32 capacity, u64 pool ref}.
fn arr5c(m: &Meta, o: usize) -> (usize, usize) {
    let n = m.r.u32(o) as usize;
    match m.ptr(m.r.u64(o + 8)) {
        Some((_, p)) if n > 0 => (p, n),
        _ => (0, 0),
    }
}

/// Junctions (`eba8aebb`, 88 B): +0x00 node key, +0x08 entrances
/// (`abc0cc55`, 80 B: +0x08 turn paths `badd6def` 176 B, +0x18 node key,
/// +0x46 lane, +0x47 bIsStop, +0x48 bIsYield), +0x28 phases (`aef55528`,
/// 24 B: +0 fDuration, +0x08 lights `c47cb7b3` 16 B: u64 hash, u8 boxId).
pub fn junctions(r: &Resource, nodes: &[Node]) -> Vec<Junction> {
    let Some(m) = Meta::parse(r) else { return vec![] };
    let by: std::collections::HashMap<[u16; 2], usize> = nodes.iter().enumerate().map(|(i, n)| (n.key, i)).collect();
    let key = |o: usize| by.get(&[r.u16(o), r.u16(o + 2)]).copied();
    let v3 = |o: usize| [r.f32(o), r.f32(o + 4), r.f32(o + 8)];
    m.objects(JUNCTION)
        .into_iter()
        .map(|o| {
            let (p, n) = arr5c(&m, o + 8);
            let entrances = (0..n)
                .map(|k| {
                    let e = p + 80 * k;
                    let (pp, pn) = arr5c(&m, e + 8);
                    let paths = (0..pn)
                        .map(|t| {
                            let q = pp + 176 * t;
                            JunctionPath { to: key(q + 0x78), start: v3(q + 0x90), end: v3(q + 0x9C), length: r.f32(q + 0x8C), lane: r.sys[q + 0xA8] }
                        })
                        .collect();
                    Entrance { node: key(e + 0x18), lane: r.sys[e + 0x46], stop: r.sys[e + 0x47] != 0, yield_: r.sys[e + 0x48] != 0, paths }
                })
                .collect();
            let (q, nq) = arr5c(&m, o + 0x28);
            let phases = (0..nq)
                .map(|t| {
                    let ph = q + 24 * t;
                    let (cp, cn) = arr5c(&m, ph + 8);
                    Phase { duration: r.f32(ph), lights: (0..cn).map(|i| (format!("{:016x}", r.u64(cp + 16 * i)), r.sys[cp + 16 * i + 8])).collect() }
                })
                .collect();
            Junction { node: key(o), entrances, phases, u38: r.u32(o + 0x38) as i32 }
        })
        .collect()
}
