//! RAGE gen9 drawable / fragment / drawable-dictionary geometry decoder
//! (port of rpf9-explorer RageModelDecoder.cs).

use crate::bytes::{le16, le32, le64};
use std::collections::HashSet;

pub const MAX_SYSTEM: usize = 32 << 20;
pub const MAX_GRAPHICS: usize = 224 << 20;
const MAX_MESHES: usize = 1024;
const MAX_VERTICES: usize = 2_000_000;
const MAX_TRIANGLES: usize = 2_000_000;
const MAX_CHILDREN: usize = 4096;
const SYS: u64 = 0x5000_0000;
const GFX: u64 = 0x6000_0000;

pub type V3 = [f32; 3];

pub struct Mesh {
    pub name: String,
    pub positions: Vec<V3>,
    pub indices: Vec<u32>,
    pub uvs: Option<Vec<[f32; 2]>>,
    /// Packed 0xAABBGGRR per vertex.
    pub colors: Option<Vec<u32>>,
    pub shader: Option<usize>,
    /// Bone name hashes of the owning model's palette.
    pub palette: Option<Vec<u32>>,
}

pub struct TexRef {
    pub param: u32,
    pub hash: u32,
    /// 64-bit asset key of the keyed texture descriptor (resolves through the ttdb pools).
    pub key: u64,
    /// 64-bit texture hash (TTPF store key), 24 bytes before the asset.
    pub h64: u64,
}

pub struct Material {
    pub shader_hash: u32,
    /// Render bucket bits (record +0x3C): 0x01 opaque, 0x02 alpha, 0x04 decal, 0x08 cutout, 0x40 water.
    pub bucket: u8,
    pub textures: Vec<TexRef>,
}

pub struct Model {
    pub name: String,
    pub meshes: Vec<Mesh>,
    pub min: V3,
    pub max: V3,
    pub fragment: bool,
    pub dictionary_child: bool,
    pub bones: bool,
    pub drawable: usize,
    /// Bounds stored in the drawable header (before recomputation).
    pub declared: (V3, V3),
    pub note: Option<&'static str>,
    pub materials: Option<Vec<Material>>,
    /// Embedded texture dictionary (texture module parser).
    pub embedded: Option<crate::texture::Embedded>,
}

pub struct Extent {
    pub system: usize,
    pub graphics: usize,
    pub complete: bool,
}

fn q64(d: &[u8], at: usize) -> u64 {
    le64(d, at).unwrap_or(0)
}
fn q16(d: &[u8], at: usize) -> u16 {
    le16(d, at).unwrap_or(0)
}
fn qf(d: &[u8], at: usize) -> f32 {
    le32(d, at).map_or(f32::NAN, f32::from_bits)
}
fn vec3(d: &[u8], at: usize) -> V3 {
    [qf(d, at), qf(d, at + 4), qf(d, at + 8)]
}
fn system_pointer(p: u64) -> bool {
    (SYS..SYS + MAX_SYSTEM as u64).contains(&p)
}
fn finite(p: V3) -> bool {
    p.iter().all(|c| c.is_finite() && c.abs() < 10_000_000.0)
}
fn valid_bounds(min: V3, max: V3) -> bool {
    finite(min) && finite(max) && (0..3).all(|i| min[i] <= max[i])
}

pub fn looks_like_fragment(p: &[u8]) -> bool {
    if p.len() < 176 || q64(p, 0) != 0 {
        return false;
    }
    // rpf9-explorer 1.2: +16/+32/+40 are optional (null or a system pointer).
    if ![8, 24, 168].iter().all(|&at| system_pointer(q64(p, at))) || ![16, 32, 40].iter().all(|&at| q64(p, at) == 0 || system_pointer(q64(p, at))) {
        return false;
    }
    let (a, b, c, d) = (q16(p, 48), q16(p, 50), q16(p, 52), q16(p, 54));
    (1..=4096).contains(&a) && b <= 4096 && c <= d && d <= 4096
}

pub fn looks_like_dictionary(p: &[u8]) -> bool {
    if p.len() < 64 || q64(p, 0) != 0 || ![8, 32, 48].iter().all(|&at| system_pointer(q64(p, at))) {
        return false;
    }
    let count = q16(p, 56);
    count > 0
        && count as usize <= MAX_CHILDREN
        && count == q16(p, 58)
        && count == q16(p, 40)
        && count == q16(p, 42)
}

fn looks_like_drawable_header(p: &[u8], embedded: bool) -> bool {
    if p.len() < 176 || q64(p, 0) != 0 {
        return false;
    }
    let (pages, shader, high, name) = (q64(p, 8), q64(p, 16), q64(p, 80), q64(p, 168));
    if (if embedded { pages != 0 } else { !system_pointer(pages) })
        || !system_pointer(shader)
        || !system_pointer(high)
        || !(system_pointer(name) || (embedded && name == 0))
    {
        return false;
    }
    let radius = qf(p, 44);
    valid_bounds(vec3(p, 48), vec3(p, 64)) && radius.is_finite() && radius >= 0.0 && radius < 1_000_000.0
}

pub fn looks_like_drawable(p: &[u8]) -> bool {
    looks_like_drawable_header(p, false)
}

pub fn looks_like_resource(p: &[u8]) -> bool {
    looks_like_drawable(p) || looks_like_fragment(p) || looks_like_dictionary(p)
}

// ---------------------------------------------------------------- extents

enum Stop {
    Bad,
    Need(usize),
}

struct ExtentReader<'a> {
    sys: &'a [u8],
    system_end: usize,
    graphics_end: usize,
    meshes: usize,
}

fn check(c: bool) -> Result<(), Stop> {
    if c { Ok(()) } else { Err(Stop::Bad) }
}

impl ExtentReader<'_> {
    fn cover(&mut self, at: usize, size: usize) -> Result<(), Stop> {
        let end = at.checked_add(size).ok_or(Stop::Bad)?;
        check(end <= MAX_SYSTEM)?;
        self.system_end = self.system_end.max(end);
        if end > self.sys.len() {
            return Err(Stop::Need(end));
        }
        Ok(())
    }
    fn u8(&mut self, at: usize) -> Result<u8, Stop> {
        self.cover(at, 1)?;
        Ok(self.sys[at])
    }
    fn u16(&mut self, at: usize) -> Result<u16, Stop> {
        self.cover(at, 2)?;
        Ok(q16(self.sys, at))
    }
    fn u32(&mut self, at: usize) -> Result<u32, Stop> {
        self.cover(at, 4)?;
        Ok(le32(self.sys, at).unwrap_or(0))
    }
    fn u64(&mut self, at: usize) -> Result<u64, Stop> {
        self.cover(at, 8)?;
        Ok(q64(self.sys, at))
    }
    fn s(&mut self, pointer_at: usize, size: usize) -> Result<usize, Stop> {
        let p = self.u64(pointer_at)?;
        check(p >= SYS && p - SYS <= MAX_SYSTEM as u64)?;
        let at = (p - SYS) as usize;
        self.cover(at, size)?;
        Ok(at)
    }
    fn g(&mut self, pointer_at: usize, size: usize) -> Result<(), Stop> {
        let p = self.u64(pointer_at)?;
        check(p >= GFX && p - GFX <= MAX_GRAPHICS as u64 && size > 0 && size as u64 <= MAX_GRAPHICS as u64 - (p - GFX))?;
        self.graphics_end = self.graphics_end.max((p - GFX) as usize + size);
        Ok(())
    }
    fn name(&mut self, pointer_at: usize, skip: usize) -> Result<(), Stop> {
        let at = self.s(pointer_at, skip + 1)? + skip;
        let available = 256.min(self.sys.len() - at);
        let end = self.sys[at..at + available].iter().position(|&b| b == 0);
        let Some(end) = end else {
            if available < 256 {
                self.cover(at, available + 1)?;
            }
            return Err(Stop::Bad);
        };
        check(end > 0 && end < 256)?;
        self.cover(at, end + 1)?;
        let text = std::str::from_utf8(&self.sys[at..at + end]).map_err(|_| Stop::Bad)?;
        check(!text.chars().any(char::is_control))
    }
    fn buffer(&mut self, at: usize) -> Result<(), Stop> {
        self.cover(at, 64)?;
        let stride = self.u16(at + 12)? as usize;
        let bytes = self.u32(at + 8)? as usize * stride;
        check((1..=32).contains(&stride) && bytes > 0)?;
        self.g(at + 24, bytes)?;
        if self.u64(at + 40)? != 0 {
            self.s(at + 40, 32)?;
        }
        Ok(())
    }

    fn probe(&mut self) -> Result<(), Stop> {
        let dictionary = looks_like_dictionary(self.sys);
        self.cover(0, if dictionary { 64 } else { 176 })?;
        let pages = self.s(8, 16)?;
        let (sp, gp) = (self.u8(pages + 8)? as usize, self.u8(pages + 9)? as usize);
        check((1..=32).contains(&sp) && (1..=32).contains(&gp))?;
        self.cover(pages, 16 + 8 * (sp + gp))?;
        if dictionary {
            let count = self.u16(56)? as usize;
            let hashes = self.s(32, count * 4)?;
            let pointers = self.s(48, count * 8)?;
            check(count > 0 && count <= MAX_CHILDREN && hashes >= 64 && pointers >= 64)?;
            let mut children = HashSet::new();
            for i in 0..count {
                let child = self.s(pointers + i * 8, 192)?;
                check(child >= 64 && children.insert(child) && looks_like_drawable_header(&self.sys[child..], true))?;
                self.probe_drawable(child)?;
            }
        } else {
            let mut drawable = 0;
            if !looks_like_drawable(self.sys) {
                check(looks_like_fragment(self.sys))?;
                self.name(168, 0)?;
                drawable = self.s(24, 192)?;
                check(drawable != 0 && looks_like_drawable_header(&self.sys[drawable..], true))?;
            }
            self.probe_drawable(drawable)?;
        }
        check(self.graphics_end > 0)
    }

    fn probe_drawable(&mut self, drawable: usize) -> Result<(), Stop> {
        self.cover(drawable, 192)?;
        if self.u64(drawable + 168)? != 0 {
            self.name(drawable + 168, 0)?;
        }
        let mut seen_lods = HashSet::new();
        for field in (80..=104).step_by(8) {
            if self.u64(drawable + field)? == 0 {
                check(field != 80)?;
                continue;
            }
            let lod = self.s(drawable + field, 16)?;
            let models = self.u16(lod + 8)? as usize;
            if !seen_lods.insert(lod) {
                continue;
            }
            check(models > 0 && models <= 256 && models == self.u16(lod + 10)? as usize)?;
            let pointers = self.s(lod, models * 8)?;
            let (mut seen_models, mut seen_geometry) = (HashSet::new(), HashSet::new());
            for mi in 0..models {
                let model = self.s(pointers + mi * 8, 56)?;
                let count = self.u16(model + 16)? as usize;
                check(
                    seen_models.insert(model)
                        && count > 0
                        && count <= MAX_MESHES
                        && count == self.u16(model + 18)? as usize
                        && count == self.u16(model + 54)? as usize,
                )?;
                let bones = self.u16(model + 20)? as usize;
                check(bones <= 4096)?;
                let geometries = self.s(model + 8, count * 8)?;
                self.s(model + 24, (count + usize::from(count > 1)) * 32)?;
                if self.u64(model + 40)? != 0 {
                    self.s(model + 40, if bones > 0 { bones * 4 } else { 1 })?;
                }
                for i in 0..count {
                    self.meshes += 1;
                    check(self.meshes <= MAX_MESHES)?;
                    let geometry = self.s(geometries + i * 8, 160)?;
                    check(seen_geometry.insert(geometry))?;
                    let vb = self.s(geometry + 8, 80)?;
                    let fvf = self.s(vb + 64, 320)?;
                    let flags = self.u32(fvf + 0x138)?;
                    let vertices = (flags >> 10) as usize;
                    let (size, format) = (self.u8(fvf + 0xD0)? as usize, self.u8(fvf + 0x104)?);
                    check(
                        flags & 1 != 0
                            && vertices > 0
                            && vertices <= 1_000_000
                            && ((size == 12 && format == 6) || (size == 8 && format == 16)),
                    )?;
                    let bytes = self.u32(vb + 8)? as usize * self.u16(vb + 12)? as usize;
                    let offset = self.u32(fvf)? as usize;
                    check(offset >= 96 && offset <= bytes && vertices * size <= bytes - offset)?;
                    self.buffer(vb)?;
                    let descriptor = self.s(geometry + 64, 16)?;
                    let ib = self.s(descriptor, 64)?;
                    check(self.u16(ib + 12)? == 4)?;
                    self.buffer(ib)?;
                }
            }
        }
        let shader = self.s(drawable + 16, 64)?;
        if self.u64(shader + 48)? != 0 {
            let descriptor = self.s(shader + 48, 32)?;
            let b = self.s(descriptor, 64)?;
            self.buffer(b)?;
        }
        if self.u64(shader + 8)? != 0 {
            let d = self.s(shader + 8, 64)?;
            self.texture_dictionary(d)?;
        }
        self.probe_materials(shader)?;
        if self.u64(drawable + 176)? != 0 {
            let renderer = self.s(drawable + 176, 80)?;
            for i in 0..5 {
                let list = renderer + i * 16;
                let count = self.u16(list + 8)? as usize;
                check(count == self.u16(list + 10)? as usize && count <= 4096)?;
                if count > 0 {
                    self.s(list, if i < 4 { count * 4 } else { 1 })?;
                }
            }
        }
        Ok(())
    }

    fn probe_materials(&mut self, group: usize) -> Result<(), Stop> {
        let count = self.u16(group + 24)? as usize;
        if count == 0 {
            return Ok(());
        }
        check(count <= 512 && count == self.u16(group + 26)? as usize)?;
        let pointers = self.s(group + 16, count * 8)?;
        for i in 0..count {
            let shader = self.s(pointers + i * 8, 64)?;
            if self.u32(shader + 4)? != 0x6D65_7461 {
                continue;
            }
            let info = self.s(shader + 32, 8)?;
            let (parameters, textures) = (self.u8(info + 4)? as usize, self.u8(info + 1)? as usize);
            self.cover(info, 8 + parameters * 8)?;
            if textures == 0 {
                continue;
            }
            let array = self.s(shader + 16, textures * 8)?;
            for j in 0..textures {
                if self.u64(array + j * 8)? == 0 {
                    continue;
                }
                let texture = self.s(array + j * 8, 80)?;
                if self.u64(texture + 48)? == 0 {
                    continue;
                }
                let asset = self.s(texture + 48, 16)?;
                if asset >= 16 && self.u32(asset - 16)? == 6 {
                    self.cover(asset - 16, 32)?;
                }
            }
        }
        Ok(())
    }

    fn texture_dictionary(&mut self, dictionary: usize) -> Result<(), Stop> {
        let count = self.u16(dictionary + 56)? as usize;
        check(count <= 4096 && count == self.u16(dictionary + 58)? as usize)?;
        if count == 0 {
            return Ok(());
        }
        check(
            dictionary >= 64
                && self.u16(dictionary + 40)? as usize == count
                && self.u16(dictionary + 42)? as usize == count,
        )?;
        check(self.s(dictionary + 32, count * 4)? >= 64)?;
        let array = self.s(dictionary + 48, count * 8)?;
        check(array >= 64)?;
        for i in 0..count {
            let texture = self.s(array + i * 8, 112)?;
            check(texture >= 64)?;
            if self.u64(texture + 40)? != 0 {
                check(self.s(texture + 40, 32)? >= 64)?;
            }
            let name = self.s(texture + 48, 9)?;
            check(name >= 64)?;
            if self.u64(name)? == 0 {
                self.name(texture + 48, 8)?;
            } else {
                check(name >= 16)?;
                self.cover(name - 16, 32)?;
            }
        }
        if let Some(required) = super::texture::required_graphics(self.sys, dictionary) {
            check(required <= MAX_GRAPHICS)?;
            self.graphics_end = self.graphics_end.max(required);
        }
        Ok(())
    }
}

/// How many system/graphics bytes the resource needs, or None if it is not a model.
pub fn probe_extents(sys: &[u8]) -> Option<Extent> {
    if sys.len() > MAX_SYSTEM {
        return None;
    }
    if sys.len() < 176 {
        let ok = sys.len() >= 16 && q64(sys, 0) == 0 && system_pointer(q64(sys, 8));
        return ok.then_some(Extent { system: 176, graphics: 0, complete: false });
    }
    if !looks_like_resource(sys) {
        return None;
    }
    let mut r = ExtentReader { sys, system_end: 0, graphics_end: 0, meshes: 0 };
    match r.probe() {
        Ok(()) => Some(Extent { system: r.system_end, graphics: r.graphics_end, complete: true }),
        Err(Stop::Need(n)) => {
            Some(Extent { system: n.max(r.system_end), graphics: r.graphics_end, complete: false })
        }
        Err(Stop::Bad) => None,
    }
}

// ---------------------------------------------------------------- decoding

struct Bad;
type R<T> = Result<T, Bad>;

fn req(c: bool) -> R<()> {
    if c { Ok(()) } else { Err(Bad) }
}
fn r8(d: &[u8], at: usize) -> R<u8> {
    d.get(at).copied().ok_or(Bad)
}
fn r16(d: &[u8], at: usize) -> R<u16> {
    le16(d, at).ok_or(Bad)
}
fn r32(d: &[u8], at: usize) -> R<u32> {
    le32(d, at).ok_or(Bad)
}
fn r64(d: &[u8], at: usize) -> R<u64> {
    le64(d, at).ok_or(Bad)
}
fn rv(d: &[u8], at: usize) -> R<V3> {
    req(at.checked_add(12).is_some_and(|e| e <= d.len()))?;
    Ok(vec3(d, at))
}

struct ModelReader<'a> {
    sys: &'a [u8],
    gfx: &'a [u8],
    vertices: usize,
    triangles: usize,
    bones: bool,
}

pub struct Child {
    pub drawable: usize,
    pub name: String,
}

impl<'a> ModelReader<'a> {
    fn new(sys: &'a [u8], gfx: &'a [u8]) -> Self {
        Self { sys, gfx, vertices: 0, triangles: 0, bones: false }
    }
    fn pointer(&self, at: usize, basis: u64, length: usize, size: usize) -> R<usize> {
        let p = r64(self.sys, at)?;
        req(p >= basis && p - basis <= length as u64 && size as u64 <= length as u64 - (p - basis))?;
        Ok((p - basis) as usize)
    }
    fn s(&self, at: usize, size: usize) -> R<usize> {
        self.pointer(at, SYS, self.sys.len(), size)
    }
    fn g(&self, at: usize, size: usize) -> R<usize> {
        self.pointer(at, GFX, self.gfx.len(), size)
    }
    fn read_name(&self, pointer_at: usize) -> R<String> {
        let at = self.s(pointer_at, 1)?;
        let available = 256.min(self.sys.len() - at);
        let end = self.sys[at..at + available].iter().position(|&b| b == 0).ok_or(Bad)?;
        req(end > 0)?;
        let text = std::str::from_utf8(&self.sys[at..at + end]).map_err(|_| Bad)?;
        req(!text.chars().any(char::is_control))?;
        Ok(text.to_owned())
    }

    fn read_dictionary(&self) -> R<Vec<Child>> {
        req(looks_like_dictionary(self.sys))?;
        let count = q16(self.sys, 56) as usize;
        let pointers = self.s(48, count * 8)?;
        let hashes = self.s(32, count * 4)?;
        req(pointers >= 64 && hashes >= 64)?;
        self.s(8, 16)?;
        let mut seen = HashSet::new();
        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            let child = self.s(pointers + i * 8, 192)?;
            req(child >= 64 && seen.insert(child) && looks_like_drawable_header(&self.sys[child..], true))?;
            result.push(Child { drawable: child, name: self.read_name(child + 168)? });
        }
        Ok(result)
    }

    fn decode(&mut self, selected: Option<usize>) -> R<Model> {
        let page_info = self.s(8, 16)?;
        let (sp, gp) = (self.sys[page_info + 8], self.sys[page_info + 9]);
        req((1..=32).contains(&sp) && (1..=32).contains(&gp))?;
        let dictionary = looks_like_dictionary(self.sys);
        let fragment = !dictionary && !looks_like_drawable(self.sys);
        let mut drawable = 0;
        let mut parent = None;
        if dictionary {
            drawable = selected.ok_or(Bad)?;
            req(drawable >= 64
                && drawable + 192 <= self.sys.len()
                && looks_like_drawable_header(&self.sys[drawable..], true))?;
        } else if fragment {
            req(looks_like_fragment(self.sys))?;
            parent = Some(self.read_name(168)?);
            drawable = self.s(24, 176)?;
            req(drawable != 0 && looks_like_drawable_header(&self.sys[drawable..], true))?;
        }
        if !dictionary && let Some(s) = selected {
            req(s == drawable)?;
        }
        let name_pointer = r64(self.sys, drawable + 168)?;
        let name = match (&parent, name_pointer) {
            (Some(p), 0) => p.clone(),
            _ => self.read_name(drawable + 168)?,
        };
        if let (Some(p), true) = (&parent, name_pointer != 0) {
            req(name.strip_suffix(".#ft") == Some(p.as_str()))?;
        }
        let (min, max) = (rv(self.sys, drawable + 48)?, rv(self.sys, drawable + 64)?);
        let lod = self.s(drawable + 80, 16)?;
        let model_count = r16(self.sys, lod + 8)? as usize;
        req(model_count > 0 && model_count <= 256 && model_count == r16(self.sys, lod + 10)? as usize)?;
        let list = self.s(lod, model_count * 8)?;
        let materials = self.read_materials(drawable).ok();
        let embedded = crate::texture::embedded_dictionary(self.sys, self.gfx, drawable);
        let mut meshes = Vec::new();
        let (mut seen_models, mut seen_geometry) = (HashSet::new(), HashSet::new());
        for mi in 0..model_count {
            let model = self.s(list + mi * 8, 56)?;
            req(seen_models.insert(model))?;
            let count = r16(self.sys, model + 16)? as usize;
            req(count > 0
                && count <= MAX_MESHES
                && count == r16(self.sys, model + 18)? as usize
                && count == r16(self.sys, model + 54)? as usize)?;
            let bone_count = r16(self.sys, model + 20)? as usize;
            req(bone_count <= 4096)?;
            self.bones |= bone_count > 0;
            let palette = if bone_count > 0 { super::skeleton::model_palette(self.sys, model) } else { None };
            let geometries = self.s(model + 8, count * 8)?;
            let mapping = materials.as_ref().and_then(|m| self.shader_mapping(model, count, m.len()).ok());
            let extra = usize::from(count > 1);
            let bounds = self.s(model + 24, (count + extra) * 32)?;
            for gi in 0..count {
                req(meshes.len() < MAX_MESHES)?;
                let geometry = self.s(geometries + gi * 8, 160)?;
                req(seen_geometry.insert(geometry))?;
                let bound = bounds + (gi + extra) * 32;
                let (part_min, part_max) = (rv(self.sys, bound)?, rv(self.sys, bound + 16)?);
                req(valid_bounds(part_min, part_max))?;
                let mut mesh = self.read_mesh(geometry, format!("model_{mi}_geometry_{gi}"), part_min, part_max, bone_count)?;
                mesh.palette.clone_from(&palette);
                if let Some(m) = mapping {
                    mesh.shader = Some(r16(self.sys, m + gi * 2)? as usize);
                }
                meshes.push(mesh);
            }
        }
        req(!meshes.is_empty())?;
        let (mut lo, mut hi) = (min, max);
        // rpf9-explorer 1.2: zero-size declared bounds fall back to the decoded geometry.
        if dictionary || self.bones || min == max {
            lo = [f32::INFINITY; 3];
            hi = [f32::NEG_INFINITY; 3];
            for mesh in &meshes {
                for &i in &mesh.indices {
                    let p = mesh.positions[i as usize];
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
            }
            req(valid_bounds(lo, hi))?;
        }
        let note = if dictionary {
            Some("Native drawable dictionary child in raw bind space; skeleton, face morphs, animation and full character assembly are not applied.")
        } else if fragment {
            Some("Raw bind-pose geometry; bone transforms, physics and fragment assembly are not applied.")
        } else if self.bones {
            Some("Raw bind-pose geometry; bone transforms are not applied.")
        } else {
            None
        };
        Ok(Model {
            name,
            meshes,
            min: lo,
            max: hi,
            fragment,
            dictionary_child: dictionary,
            bones: self.bones,
            drawable,
            declared: (min, max),
            note,
            materials,
            embedded,
        })
    }

    fn shader_mapping(&self, model: usize, count: usize, materials: usize) -> R<usize> {
        let mapping = self.s(model + 32, count * 2)?;
        for i in 0..count {
            req((r16(self.sys, mapping + i * 2)? as usize) < materials)?;
        }
        Ok(mapping)
    }

    fn read_materials(&self, drawable: usize) -> R<Vec<Material>> {
        let sys = self.sys;
        let group = self.s(drawable + 16, 64)?;
        let count = r16(sys, group + 24)? as usize;
        req(count > 0 && count <= 512 && count == r16(sys, group + 26)? as usize)?;
        let pointers = self.s(group + 16, count * 8)?;
        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            let shader = self.s(pointers + i * 8, 64)?;
            let hash = r32(sys, shader)?;
            req(r32(sys, shader + 4)? == 0x6D65_7461)?;
            let info = self.s(shader + 32, 8)?;
            let (textures, parameters) = (sys[info + 1] as usize, sys[info + 4] as usize);
            req(sys[info + 5] == 0 && sys[info + 6] == 0 && (1..=16).contains(&sys[info + 7]))?;
            req(info + 8 + parameters * 8 <= sys.len())?;
            let array = if textures > 0 { self.s(shader + 16, textures * 8)? } else { 0 };
            let mut refs = Vec::new();
            let mut seen = HashSet::new();
            for p in 0..parameters {
                let at = info + 8 + p * 8;
                let data = r32(sys, at + 4)?;
                if data & 3 != 0 {
                    continue;
                }
                let index = ((data >> 2) & 255) as usize;
                req(index < textures && seen.insert(index))?;
                if r64(sys, array + index * 8)? == 0 {
                    continue;
                }
                let texture = self.s(array + index * 8, 80)?;
                if r32(sys, texture + 16)? != 0x980
                    || r32(sys, texture + 20)? != 0
                    || r64(sys, texture + 24)? != 0
                    || r64(sys, texture + 40)? != 0
                    || r32(sys, texture + 56)? != 0
                    || r16(sys, texture + 60)? != 1
                    || r16(sys, texture + 62)? != 1
                    || r8(sys, texture + 64)? != 255
                    || r64(sys, texture + 48)? == 0
                {
                    continue;
                }
                let asset = self.s(texture + 48, 16)?;
                if asset < 16
                    || r32(sys, asset - 16)? != 6
                    || r32(sys, asset - 12)? != 0
                    || r32(sys, asset - 4)? != 0
                    || r64(sys, asset)? != SYS + asset as u64 - 11
                    || r64(sys, asset + 8)? == 0
                {
                    continue;
                }
                refs.push(TexRef { param: r32(sys, at)?, hash: r32(sys, asset - 8)?, key: r64(sys, asset + 8)?, h64: r64(sys, asset - 24).unwrap_or(0) });
            }
            result.push(Material { shader_hash: hash, bucket: sys.get(shader + 0x3C).copied().unwrap_or(0), textures: refs });
        }
        Ok(result)
    }

    fn read_colors(&self, fvf: usize, count: usize, vertex: &[u8]) -> Option<Vec<u32>> {
        const SEMANTIC: usize = 24;
        if *self.sys.get(fvf + 0xD0 + SEMANTIC)? != 4 || *self.sys.get(fvf + 0x104 + SEMANTIC)? != 28 || vertex.len() < 8 {
            return None;
        }
        let offset = le32(self.sys, fvf + SEMANTIC * 4)? as usize;
        let native = le32(vertex, 4)? as usize;
        if offset > vertex.len()
            || count * 4 > vertex.len() - offset
            || (native >> 7) & 0x1FF_FFFC != offset
            || (native >> 3) & 60 != 4
            || native & 15 != 6
        {
            return None;
        }
        (0..count).map(|i| le32(vertex, offset + i * 4)).collect()
    }

    fn read_uvs(&self, fvf: usize, count: usize, vertex: &[u8]) -> Option<Vec<[f32; 2]>> {
        const SEMANTIC: usize = 28;
        let size = *self.sys.get(fvf + 0xD0 + SEMANTIC)? as usize;
        let format = *self.sys.get(fvf + 0x104 + SEMANTIC)?;
        let floating = size == 8 && format == 16;
        let normalized = size == 4 && format == 38;
        if !floating && !normalized {
            return None;
        }
        let offset = le32(self.sys, fvf + SEMANTIC * 4)? as usize;
        if offset > vertex.len() || count * size > vertex.len() - offset || vertex.len() < 16 {
            return None;
        }
        let native = le32(vertex, 12)? as usize;
        if (native >> 7) & 0x1FF_FFFC != offset
            || (native >> 3) & 60 != size
            || native & 15 != if floating { 2 } else { 8 }
        {
            return None;
        }
        let mut scale = 1.0;
        if normalized {
            scale = f32::from_bits(le32(vertex, 124)?);
            if !scale.is_finite() || scale <= 0.0 || scale > 1_000_000.0 {
                return None;
            }
        }
        let snorm = |at| (le16(vertex, at).unwrap_or(0) as i16 as f32 / 32767.0).max(-1.0) * scale;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let at = offset + i * size;
            let (u, v) = if floating { (qf(vertex, at), qf(vertex, at + 4)) } else { (snorm(at), snorm(at + 2)) };
            if !u.is_finite() || !v.is_finite() || u.abs() > 1_000_000.0 || v.abs() > 1_000_000.0 {
                return None;
            }
            out.push([u, v]);
        }
        Some(out)
    }

    fn read_mesh(&mut self, geometry: usize, name: String, bmin: V3, bmax: V3, bone_count: usize) -> R<Mesh> {
        let sys = self.sys;
        let vb = self.s(geometry + 8, 80)?;
        let fvf = self.s(vb + 64, 320)?;
        let flags = r32(sys, fvf + 0x138)?;
        let vertex_count = (flags >> 10) as usize;
        req(flags & 1 != 0 && vertex_count > 0 && vertex_count <= 1_000_000)?;
        self.vertices += vertex_count;
        req(self.vertices <= MAX_VERTICES)?;
        let position_offset = r32(sys, fvf)? as usize;
        let (position_size, format) = (sys[fvf + 0xD0] as usize, sys[fvf + 0x104]);
        let length = r32(sys, vb + 8)? as usize * r16(sys, vb + 12)? as usize;
        req(length > 0
            && position_offset >= 96
            && position_offset <= length
            && position_size * vertex_count <= length - position_offset)?;
        let data = self.g(vb + 24, length)?;
        let vertex = &self.gfx[data..data + length];
        let mut positions = Vec::with_capacity(vertex_count);
        let mut quantized = None;
        if position_size == 12 && format == 6 {
            req(r32(vertex, 0)? & 127 == 0x63)?;
            for i in 0..vertex_count {
                positions.push(rv(vertex, position_offset + i * 12)?);
            }
        } else if position_size == 8 && format == 16 {
            req(position_offset >= 128 && r32(vertex, 0)? & 127 == 0x42)?;
            let (bias, scale, axis) = (rv(vertex, 96)?, rv(vertex, 112)?, r32(vertex, 108)?);
            req(axis <= 2 && finite(bias) && finite(scale) && scale.iter().all(|&s| s >= 0.0))?;
            let span = [16_777_215.0, 16_777_215.0, 65535.0];
            let end = [0, 1, 2].map(|k| bias[k] + scale[k] * span[k]);
            quantized = Some((reorder(bias, axis), reorder(end, axis)));
            for i in 0..vertex_count {
                let q = r64(vertex, position_offset + i * 8)?;
                let p = [
                    (bias[0] as f64 + scale[0] as f64 * ((q >> 8) & 0xFF_FFFF) as f64) as f32,
                    (bias[1] as f64 + scale[1] as f64 * ((((q >> 32) & 0xFFFF) << 8) | (q & 0xFF)) as f64) as f32,
                    (bias[2] as f64 + scale[2] as f64 * (q >> 48) as f64) as f32,
                ];
                positions.push(reorder(p, axis));
            }
        } else {
            return Err(Bad);
        }
        req(positions.iter().all(|&p| finite(p)))?;

        let descriptor = self.s(geometry + 64, 16)?;
        let ib = self.s(descriptor, 64)?;
        let index_length = r32(sys, ib + 8)? as usize * r16(sys, ib + 12)? as usize;
        req(r16(sys, ib + 12)? == 4 && index_length >= 32)?;
        let index_at = self.g(ib + 24, index_length)?;
        let index = &self.gfx[index_at..index_at + index_length];
        let count_flags = r32(index, 0)?;
        let bone_palette = count_flags & 0x4000_0000 != 0;
        let clusters = (count_flags & if bone_palette { 0xFFFF } else { 0x7FFF_FFFF }) as usize;
        let (triangles, refs) = (r32(index, 4)? as usize, r32(index, 8)? as usize);
        let binding = r32(sys, descriptor + 12)?;
        let palette_without_marker = binding & 255 == 0 && count_flags & 0xFFFF_0000 == 0xC000_0000;
        req((1..=2048).contains(&clusters)
            && (1..=65535).contains(&triangles)
            && (1..=65535).contains(&refs)
            && clusters == r16(sys, descriptor + 8)? as usize
            && triangles == r16(sys, descriptor + 10)? as usize
            && refs == ((binding >> 8) & 0x7F_FFFF) as usize
            && (binding & 0x8000_0000 != 0) == bone_palette
            && (!bone_palette || binding & 255 == 0x80 || palette_without_marker))?;
        self.bones |= bone_palette;
        self.triangles += triangles;
        req(self.triangles <= MAX_TRIANGLES)?;
        let ref_size = if count_flags & 0x8000_0000 != 0 { 2 } else { 4 };
        let triangle_at = 12 + clusters * 16;
        let refs_at = triangle_at + triangles * 4;
        req(refs_at <= index_length && refs * ref_size <= index_length - refs_at)?;
        let palette_at = refs_at + refs * ref_size;
        req(palette_at + if bone_palette { clusters * 2 } else { 0 } <= index_length)?;
        if bone_palette {
            let hint = ((count_flags >> 16) & 0x1FFF) as usize;
            let constant = count_flags & 0x2000_0000 != 0;
            // 16-bit bone indices (R16_UINT in the blend-index slot) carry hint 2.
            let wide = r8(sys, fvf + 0xD0 + 20)? == 2 && r8(sys, fvf + 0x104 + 20)? == 57;
            req(if constant { hint & 1 == 0 && (bone_count == 0 || hint / 2 < bone_count) } else { hint == 0 || (hint == 2 && wide) })?;
            for i in 0..clusters {
                let bone = r16(index, palette_at + i * 2)? as usize;
                req(bone == 0xFFFF || bone_count == 0 || bone < bone_count)?;
                if constant {
                    req(bone == hint / 2)?;
                }
            }
        }
        let mut references = Vec::with_capacity(refs);
        for i in 0..refs {
            let r = if ref_size == 2 { r16(index, refs_at + i * 2)? as usize } else { r32(index, refs_at + i * 4)? as usize };
            req(r < vertex_count)?;
            references.push(r as u32);
        }
        let mut indices = vec![0u32; triangles * 3];
        let (mut acc_refs, mut acc_tris) = (0, 0);
        let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for cluster in 0..clusters {
            let at = 12 + cluster * 16;
            let (packed, offsets) = (r32(index, at)?, r32(index, at + 4)?);
            let vertices = (packed & 127) as usize + 1;
            let count = ((packed >> 7) & 255) as usize + 1;
            let (vstart, tstart) = ((offsets & 65535) as usize, (offsets >> 16) as usize);
            req(vstart == acc_refs && tstart == acc_tris && vertices <= refs - vstart && count <= triangles - tstart)?;
            for i in tstart..tstart + count {
                let word = r32(index, triangle_at + i * 4)?;
                req((word >> 21) as usize == cluster)?;
                for corner in 0..3 {
                    let local = ((word >> (corner * 7)) & 127) as usize;
                    req(local < vertices)?;
                    let v = references[vstart + local];
                    indices[i * 3 + corner] = v;
                    let p = positions[v as usize];
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
            }
            acc_refs += vertices;
            acc_tris += count;
        }
        req(acc_refs == refs && acc_tris == triangles)?;
        let tolerance = 0.002 + (0..3).map(|k| bmax[k] - bmin[k]).fold(f32::MIN, f32::max) * 0.00001;
        let fit = if quantized.is_some() { tolerance.max(0.01001) } else { tolerance };
        let close = |a: V3, b: V3, t: f32| (0..3).all(|k| (a[k] - b[k]).abs() <= t);
        let contains = |p: V3| (0..3).all(|k| p[k] >= bmin[k] - tolerance && p[k] <= bmax[k] + tolerance);
        let tight = close(lo, bmin, fit) && close(hi, bmax, fit);
        let conservative = quantized.is_some_and(|(qmin, qmax)| {
            close(qmin, bmin, tolerance) && close(qmax, bmax, tolerance) && contains(lo) && contains(hi)
        });
        req(tight || conservative)?;
        let uvs = self.read_uvs(fvf, vertex_count, vertex);
        let colors = self.read_colors(fvf, vertex_count, vertex);
        Ok(Mesh { name, positions, indices, uvs, colors, shader: None, palette: None })
    }
}

fn reorder(p: V3, axis: u32) -> V3 {
    match axis {
        0 => [p[2], p[1], p[0]],
        1 => [p[1], p[2], p[0]],
        _ => p,
    }
}

pub fn dictionary_children(sys: &[u8]) -> Option<Vec<Child>> {
    if sys.len() > MAX_SYSTEM {
        return None;
    }
    ModelReader::new(sys, &[]).read_dictionary().ok()
}

/// Decode every drawable in a resource (TryDecodeResource semantics: a
/// dictionary is accepted only if every child decodes).
pub fn decode_resource(sys: &[u8], gfx: &[u8]) -> Option<Vec<Model>> {
    if sys.len() > MAX_SYSTEM || gfx.is_empty() || gfx.len() > MAX_GRAPHICS || !looks_like_resource(sys) {
        return None;
    }
    if !looks_like_dictionary(sys) {
        return ModelReader::new(sys, gfx).decode(None).ok().map(|m| vec![m]);
    }
    let children = dictionary_children(sys)?;
    let mut out = Vec::with_capacity(children.len());
    let (mut meshes, mut vertices, mut triangles) = (0, 0, 0);
    for child in &children {
        let model = ModelReader::new(sys, gfx).decode(Some(child.drawable)).ok()?;
        if model.name != child.name {
            return None;
        }
        meshes += model.meshes.len();
        vertices += model.meshes.iter().map(|m| m.positions.len()).sum::<usize>();
        triangles += model.meshes.iter().map(|m| m.indices.len() / 3).sum::<usize>();
        if meshes > MAX_MESHES || vertices > MAX_VERTICES || triangles > MAX_TRIANGLES {
            return None;
        }
        out.push(model);
    }
    Some(out)
}
