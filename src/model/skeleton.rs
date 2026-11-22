//! Drawable skeleton reader (port of RageSkeleton.cs).

use crate::bytes::{le16, le32, le64};

const MAX_JOINTS: usize = 4096;
const BONE_STRIDE: usize = 0x50;

pub struct Joint {
    pub hash: u32,
    pub parent: Option<usize>,
    /// Row-major 4x4 inverse bind matrix.
    pub inverse_bind: [f32; 16],
}

fn pointer(sys: &[u8], field: usize, size: usize) -> Option<usize> {
    let p = le64(sys, field)?;
    if !(0x5000_0000..0x6000_0000).contains(&p) {
        return None;
    }
    let at = (p - 0x5000_0000) as usize;
    (at + size <= sys.len()).then_some(at)
}

pub fn read_for_drawable(sys: &[u8], drawable: usize) -> Option<Vec<Joint>> {
    let header = pointer(sys, drawable + 0x18, 0x70)?;
    read(sys, header)
}

pub fn read(sys: &[u8], header: usize) -> Option<Vec<Joint>> {
    let count = le16(sys, header + 0x6c)? as usize;
    if !(1..=MAX_JOINTS).contains(&count) || count != le16(sys, header + 0x6e)? as usize {
        return None;
    }
    let bones = pointer(sys, header + 0x28, count * BONE_STRIDE)?;
    let inverse = pointer(sys, header + 0x30, count * 64)?;
    let parents = pointer(sys, header + 0x40, count * 2)?;
    let mut joints = Vec::with_capacity(count);
    for i in 0..count {
        let bone = bones + i * BONE_STRIDE;
        let parent = le16(sys, parents + i * 2)?;
        if le16(sys, bone + 0x48)? as usize != i
            || le16(sys, bone + 0x46)? != parent
            || (parent != u16::MAX && parent as usize >= count)
        {
            return None;
        }
        let mut m = [0f32; 16];
        for (k, v) in m.iter_mut().enumerate() {
            *v = f32::from_bits(le32(sys, inverse + i * 64 + k * 4)?);
            if k != 15 && !v.is_finite() {
                return None;
            }
        }
        if m[3] != 0.0 || m[7] != 0.0 || m[11] != 0.0 {
            return None;
        }
        m[15] = 1.0;
        let det = m[0] * (m[5] * m[10] - m[6] * m[9]) - m[1] * (m[4] * m[10] - m[6] * m[8])
            + m[2] * (m[4] * m[9] - m[5] * m[8]);
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let hash = le32(sys, bone + 0x1c)?;
        joints.push(Joint { hash, parent: (parent != u16::MAX).then_some(parent as usize), inverse_bind: m });
    }
    acyclic(&joints).then_some(joints)
}

/// Bone-name hashes of a model's palette (RageSkeleton.ReadModelPalette).
pub fn model_palette(sys: &[u8], model: usize) -> Option<Vec<u32>> {
    let count = le16(sys, model + 0x14)? as usize;
    if !(1..=MAX_JOINTS).contains(&count) {
        return None;
    }
    let palette = pointer(sys, model + 0x28, count * 4)?;
    (0..count).map(|i| le32(sys, palette + i * 4)).collect()
}

/// Map palette hashes to joint indices (TryResolvePalette); None if any is
/// missing or ambiguous.
pub fn resolve_palette(joints: &[Joint], palette: &[u32]) -> Option<Vec<usize>> {
    palette
        .iter()
        .map(|h| {
            let mut found = joints.iter().enumerate().filter(|(_, j)| j.hash == *h);
            match (found.next(), found.next()) {
                (Some((i, _)), None) => Some(i),
                _ => None,
            }
        })
        .collect()
}

fn acyclic(joints: &[Joint]) -> bool {
    // Every ancestor chain must terminate within `len` steps.
    joints.iter().all(|j| {
        let (mut at, mut steps) = (j.parent, 0);
        while let Some(p) = at {
            steps += 1;
            if steps > joints.len() {
                return false;
            }
            at = joints[p].parent;
        }
        true
    })
}
