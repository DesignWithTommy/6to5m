//! Read-only GTA V (legacy PC) RPF7 archives, pure Rust: enough to list the
//! vanilla map files and read their resources (used by `fivem build` to stub
//! out Los Santos).
//!
//! Keys (port of CodeWalker GTA5Keys.UseMagicData / GTACrypto, MIT, credit
//! dexyfex): the AES key is found in GTA5.exe by SHA-1 search; it decrypts
//! CodeWalker's `magic.dat` into the NG keys, NG decrypt tables and the hash
//! LUT. The derived keys are cached in `<work>/gta5_keys.bin` (never in the
//! repo). Nothing is written back to the game.

use memmap2::Mmap;
use sha1::{Digest, Sha1};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// SHA-1 of the 32-byte PC AES key (CodeWalker GTA5KeyHashes.PC_AES_KEY_HASH).
const AES_KEY_SHA1: [u8; 20] = [0xA0, 0x79, 0x61, 0x28, 0xA7, 0x75, 0x72, 0x0A, 0xC2, 0x04, 0xD9, 0x81, 0x9F, 0x68, 0xC1, 0x72, 0xE3, 0x95, 0x2C, 0x6D];
const NG_KEYS_LEN: usize = 101 * 272;
const NG_TABLES_LEN: usize = 17 * 16 * 256 * 4;
const KEYS_LEN: usize = NG_KEYS_LEN + NG_TABLES_LEN + 256;

pub struct Keys {
    ng_keys: Vec<[u32; 68]>,
    tables: Vec<u32>, // [17][16][256]
    lut: [u8; 256],
    aes: [u8; 32],
}

impl Keys {
    fn from_blob(b: &[u8], aes: [u8; 32]) -> Result<Keys, String> {
        if b.len() < KEYS_LEN {
            return Err("key blob too short".into());
        }
        let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let ng_keys = (0..101).map(|i| std::array::from_fn(|k| u(i * 272 + k * 4))).collect();
        let tables = (0..17 * 16 * 256).map(|i| u(NG_KEYS_LEN + i * 4)).collect();
        let lut = b[NG_KEYS_LEN + NG_TABLES_LEN..KEYS_LEN].try_into().unwrap();
        Ok(Keys { ng_keys, tables, lut, aes })
    }

    /// AES-256 ECB in place (whole blocks; a tail is left as is).
    pub fn decrypt_aes(&self, data: &mut [u8]) {
        use aes::cipher::{BlockDecrypt, KeyInit};
        let c = aes::Aes256::new(&self.aes.into());
        let n = data.len() / 16 * 16;
        for blk in data[..n].chunks_exact_mut(16) {
            c.decrypt_block(blk.into());
        }
    }

    /// GTA5Hash.CalculateHash (file-name hash used to pick the NG key).
    pub fn name_hash(&self, s: &str) -> u32 {
        let mut r: u32 = 0;
        for c in s.bytes() {
            let t = 1025u32.wrapping_mul((self.lut[c as usize] as u32).wrapping_add(r));
            r = (t >> 6) ^ t;
        }
        let n = 9u32.wrapping_mul(r);
        32769u32.wrapping_mul((n >> 11) ^ n)
    }

    /// Decrypt `data` in place with the NG key for (file name, file size).
    pub fn decrypt_ng(&self, data: &mut [u8], name: &str, size: u32) {
        let key = &self.ng_keys[((self.name_hash(name).wrapping_add(size).wrapping_add(61)) % 101) as usize];
        let t = |r: usize, j: usize, x: u8| self.tables[(r * 16 + j) * 256 + x as usize];
        for blk in data.chunks_exact_mut(16) {
            let mut d: [u8; 16] = blk.try_into().unwrap();
            for r in 0..17 {
                let k = &key[r * 4..r * 4 + 4];
                let x: [u32; 4] = if r < 2 || r == 16 {
                    std::array::from_fn(|i| t(r, i * 4, d[i * 4]) ^ t(r, i * 4 + 1, d[i * 4 + 1]) ^ t(r, i * 4 + 2, d[i * 4 + 2]) ^ t(r, i * 4 + 3, d[i * 4 + 3]) ^ k[i])
                } else {
                    const P: [[usize; 4]; 4] = [[0, 7, 10, 13], [1, 4, 11, 14], [2, 5, 8, 15], [3, 6, 9, 12]];
                    std::array::from_fn(|i| P[i].iter().fold(k[i], |a, &j| a ^ t(r, j, d[j])))
                };
                for i in 0..4 {
                    d[i * 4..i * 4 + 4].copy_from_slice(&x[i].to_le_bytes());
                }
            }
            blk.copy_from_slice(&d);
        }
    }
}

/// .NET Framework System.Random (Knuth subtractive), as `new Random(seed)`.
struct NetRandom {
    s: [i32; 56],
    i: usize,
    j: usize,
}

impl NetRandom {
    fn new(seed: i32) -> Self {
        const MBIG: i32 = i32::MAX;
        let mut s = [0i32; 56];
        let sub = if seed == i32::MIN { i32::MAX } else { seed.abs() };
        let mut mj = 161_803_398 - sub;
        s[55] = mj;
        let mut mk = 1i32;
        for i in 1..55 {
            let ii = (21 * i) % 55;
            s[ii] = mk;
            mk = mj - mk;
            if mk < 0 {
                mk += MBIG;
            }
            mj = s[ii];
        }
        for _ in 1..5 {
            for i in 1..56 {
                s[i] = s[i].wrapping_sub(s[1 + (i + 30) % 55]);
                if s[i] < 0 {
                    s[i] += MBIG;
                }
            }
        }
        NetRandom { s, i: 0, j: 21 }
    }

    fn sample(&mut self) -> i32 {
        self.i = if self.i + 1 >= 56 { 1 } else { self.i + 1 };
        self.j = if self.j + 1 >= 56 { 1 } else { self.j + 1 };
        let mut r = self.s[self.i].wrapping_sub(self.s[self.j]);
        if r == i32::MAX {
            r -= 1;
        }
        if r < 0 {
            r += i32::MAX;
        }
        self.s[self.i] = r;
        r
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.sample() as u8).collect()
    }
}

fn joaat_bytes(d: &[u8]) -> u32 {
    let mut h: u32 = 0;
    for &b in d {
        h = h.wrapping_add(b as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}

fn find_aes_key(exe: &[u8]) -> Option<[u8; 32]> {
    use rayon::prelude::*;
    (0..exe.len().saturating_sub(32) / 8).into_par_iter().find_map_any(|i| {
        let w = &exe[i * 8..i * 8 + 32];
        (Sha1::digest(w).as_slice() == AES_KEY_SHA1).then(|| w.try_into().unwrap())
    })
}

/// Candidate places for CodeWalker's magic.dat: $CODEWALKER_MAGIC, then
/// `_ref/CodeWalker/CodeWalker.Core/Resources/magic.dat` next to any ancestor
/// of the given dirs (the repo's reference clones).
pub fn find_magic(dirs: &[&Path]) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CODEWALKER_MAGIC").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    for d in dirs {
        let d = fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf());
        for a in d.ancestors() {
            for sub in ["magic.dat", "_ref/CodeWalker/CodeWalker.Core/Resources/magic.dat", "CodeWalker/CodeWalker.Core/Resources/magic.dat"] {
                let p = a.join(sub);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Load the NG keys: from `cache` when it matches GTA5.exe (size + mtime),
/// else derived from GTA5.exe + magic.dat and cached.
pub fn load_keys(gta5: &Path, magic: Option<&Path>, cache: &Path) -> Result<Keys, String> {
    let exe = gta5.join("GTA5.exe");
    let m = fs::metadata(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let stamp = format!("{}:{:?}", m.len(), m.modified().ok());
    let tag = crate::gta5::fivem::sha(&stamp);
    if let Ok(b) = fs::read(cache) {
        if b.len() == 24 + 32 + KEYS_LEN && b[..24] == *tag.as_bytes() {
            return Keys::from_blob(&b[56..], b[24..56].try_into().unwrap());
        }
    }
    let magic = magic.ok_or("CodeWalker magic.dat not found (set CODEWALKER_MAGIC or keep _ref/CodeWalker)")?;
    let data = fs::read(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let key = find_aes_key(&data).ok_or("AES key not found in GTA5.exe")?;
    let mut m = fs::read(magic).map_err(|e| format!("{}: {e}", magic.display()))?;
    let mut rnd = NetRandom::new(joaat_bytes(&key) as i32);
    let r: Vec<Vec<u8>> = (0..4).map(|_| rnd.bytes(m.len())).collect();
    for (i, b) in m.iter_mut().enumerate() {
        *b = b.wrapping_sub(r[0][i]).wrapping_sub(r[1][i]).wrapping_sub(r[2][i]).wrapping_sub(r[3][i]);
    }
    Keys { ng_keys: Vec::new(), tables: Vec::new(), lut: [0; 256], aes: key }.decrypt_aes(&mut m);
    let mut blob = Vec::new();
    flate2::read::DeflateDecoder::new(&m[..]).read_to_end(&mut blob).map_err(|e| format!("magic.dat: {e} (wrong GTA5.exe?)"))?;
    let keys = Keys::from_blob(&blob, key)?;
    let mut out = tag.into_bytes();
    out.extend_from_slice(&key);
    out.extend_from_slice(&blob[..KEYS_LEN]);
    let _ = crate::gta5::fivem::write_atomic(cache, &out);
    Ok(keys)
}

/// One file in an archive (`path` lower case, '/' separated, from the archive root).
pub struct FileEntry {
    pub path: String,
    pub name: String,
    /// absolute offset in the top-level file
    pub offset: u64,
    /// stored size (0 = uncompressed binary of `usize`)
    pub size: u32,
    pub usize: u32,
    pub resource: bool,
    pub sys_flags: u32,
    pub gfx_flags: u32,
    /// binary entry encryption: 0 none, 1 AES, 2 NG (archive's scheme when
    /// the entry's encrypted flag is set; resources are never encrypted)
    pub enc: u8,
}

/// Walk one archive (and nested .rpf files for which `descend(path)` holds),
/// calling `f` for every file entry. `start`/`len` locate the archive in `data`.
pub fn walk(keys: &Keys, data: &[u8], start: u64, len: u64, name: &str, prefix: &str, descend: &dyn Fn(&str) -> bool, f: &mut dyn FnMut(&FileEntry)) -> Result<(), String> {
    let s = start as usize;
    let rd = |o: usize| -> Result<u32, String> { data.get(s + o..s + o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("{name}: truncated")) };
    if rd(0)? != 0x5250_4637 {
        return Err(format!("{name}: not RPF7"));
    }
    let (count, nlen, enc) = (rd(4)? as usize, rd(8)? as usize, rd(12)?);
    let toc = data.get(s + 16..s + 16 + count * 16 + nlen).ok_or_else(|| format!("{name}: truncated TOC"))?;
    let (mut ents, mut names) = (toc[..count * 16].to_vec(), toc[count * 16..].to_vec());
    match enc {
        0 | 0x4E45_504F => {}
        0x0FFF_FFF9 => {
            keys.decrypt_aes(&mut ents);
            keys.decrypt_aes(&mut names);
        }
        _ => {
            keys.decrypt_ng(&mut ents, name, len as u32);
            keys.decrypt_ng(&mut names, name, len as u32);
        }
    }
    let u32_ = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let ename = |no: usize| -> String {
        let n = names.get(no..).unwrap_or(&[]);
        String::from_utf8_lossy(&n[..n.iter().position(|&c| c == 0).unwrap_or(n.len())]).to_lowercase()
    };
    // directories: (entries index, count); walk from the root with paths
    let mut stack = vec![(0usize, prefix.to_string())];
    while let Some((di, dpath)) = stack.pop() {
        let e = &ents[di * 16..di * 16 + 16];
        let (first, n) = (u32_(e, 8) as usize, u32_(e, 12) as usize);
        for i in first..(first + n).min(count) {
            let e = &ents[i * 16..i * 16 + 16];
            let x = u32_(e, 4);
            if x == 0x7FFF_FF00 {
                let p = format!("{dpath}{}/", ename(u32_(e, 0) as usize));
                if descend(&p) {
                    stack.push((i, p));
                }
                continue;
            }
            let nm = ename(u16::from_le_bytes([e[0], e[1]]) as usize);
            let mut size = e[2] as u32 | (e[3] as u32) << 8 | (e[4] as u32) << 16;
            let resource = x & 0x8000_0000 != 0;
            let off24 = e[5] as u32 | (e[6] as u32) << 8 | (e[7] as u32) << 16;
            let off = (if resource { off24 & 0x7F_FFFF } else { off24 }) as u64 * 512;
            let abs = start + off;
            if resource && size == 0xFF_FFFF {
                if let Some(h) = data.get(abs as usize..abs as usize + 16) {
                    size = h[7] as u32 | (h[14] as u32) << 8 | (h[5] as u32) << 16 | (h[2] as u32) << 24;
                }
            }
            let fe = FileEntry {
                path: format!("{dpath}{nm}"),
                name: nm,
                offset: abs,
                size,
                usize: if resource { 0 } else { u32_(e, 8) },
                resource,
                sys_flags: if resource { u32_(e, 8) } else { 0 },
                gfx_flags: if resource { u32_(e, 12) } else { 0 },
                enc: match (resource, u32_(e, 12) != 0, enc) {
                    (false, true, 0x0FFF_FFF9) => 1,
                    (false, true, 0 | 0x4E45_504F) => 0,
                    (false, true, _) => 2,
                    _ => 0,
                },
            };
            if !resource && fe.name.ends_with(".rpf") && descend(&format!("{}/", fe.path)) {
                let l = if fe.size == 0 { fe.usize } else { fe.size } as u64;
                if let Err(err) = walk(keys, data, abs, l, &fe.name, &format!("{}/", fe.path), descend, f) {
                    eprintln!("rpf7: {err}");
                }
                continue;
            }
            f(&fe);
        }
    }
    Ok(())
}

/// Inflated resource data (system + graphics segments) of a resource entry.
pub fn resource_data(data: &[u8], e: &FileEntry) -> Option<Vec<u8>> {
    let b = data.get(e.offset as usize + 16..(e.offset + e.size as u64) as usize)?;
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(b).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Contents of a binary (non-resource) entry: decrypted with the archive's
/// scheme when the entry is encrypted (NG key from name + uncompressed size,
/// as CodeWalker RpfFile.ExtractFileBinary), then inflated when compressed.
pub fn binary_data(keys: &Keys, data: &[u8], e: &FileEntry) -> Option<Vec<u8>> {
    let stored = if e.size == 0 { e.usize } else { e.size } as u64;
    let mut b = data.get(e.offset as usize..(e.offset + stored) as usize)?.to_vec();
    match e.enc {
        1 => keys.decrypt_aes(&mut b),
        2 => keys.decrypt_ng(&mut b, &e.name, e.usize),
        _ => {}
    }
    if e.size == 0 {
        return Some(b);
    }
    let mut out = Vec::with_capacity(e.usize as usize);
    flate2::read::DeflateDecoder::new(&b[..]).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Map a whole file read-only.
pub fn map(p: &Path) -> Result<Mmap, String> {
    let f = fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    // SAFETY: read-only map of a game archive nothing writes while we run.
    unsafe { Mmap::map(&f) }.map_err(|e| format!("{}: {e}", p.display()))
}
