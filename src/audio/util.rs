//! Small dependency-free helpers: SHA-256, base64, and a minimal JSON value.

use std::{collections::BTreeMap, fmt::Write as _};

pub struct Sha256 {
    state: [u32; 8],
    buf: Vec<u8>,
    len: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
    0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
    0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
    0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
    0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: Vec::with_capacity(64),
            len: 0,
        }
    }

    fn block(&mut self, b: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b2, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let t1 = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)).wrapping_add((a & b2) ^ (a & c) ^ (b2 & c));
            (h, g, f, e, d, c, b2, a) = (g, f, e, d.wrapping_add(t1), c, b2, a, t1.wrapping_add(t2));
        }
        for (s, v) in self.state.iter_mut().zip([a, b2, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if !self.buf.is_empty() {
            let take = (64 - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() == 64 {
                let b = std::mem::take(&mut self.buf);
                self.block(&b);
            }
        }
        let mut chunks = data.chunks_exact(64);
        for c in &mut chunks {
            self.block(c);
        }
        self.buf.extend_from_slice(chunks.remainder());
    }

    pub fn hex(mut self) -> String {
        let bits = self.len * 8;
        let mut pad = vec![0x80u8];
        pad.resize((55usize.wrapping_sub(self.buf.len()) % 64) + 1, 0);
        pad.extend(bits.to_be_bytes());
        let len = self.len;
        self.update(&pad);
        self.len = len;
        self.state.iter().fold(String::new(), |mut s, v| {
            let _ = write!(s, "{v:08x}");
            s
        })
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.hex()
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            s.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

#[derive(Clone, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl From<&str> for Json {
    fn from(s: &str) -> Self {
        Json::Str(s.into())
    }
}
impl From<String> for Json {
    fn from(s: String) -> Self {
        Json::Str(s)
    }
}
impl From<bool> for Json {
    fn from(b: bool) -> Self {
        Json::Bool(b)
    }
}
macro_rules! num {
    ($($t:ty),*) => {$(impl From<$t> for Json { fn from(v: $t) -> Self { Json::Num(v as f64) } })*};
}
num!(u8, u16, u32, u64, usize, i32, i64, f64);
impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(v: Option<T>) -> Self {
        v.map_or(Json::Null, Into::into)
    }
}
impl From<Vec<Json>> for Json {
    fn from(v: Vec<Json>) -> Self {
        Json::Arr(v)
    }
}

/// Build an object: `obj![("k", v), ...]`.
macro_rules! obj {
    ($(($k:expr, $v:expr)),* $(,)?) => { $crate::audio::util::Json::Obj(vec![$(($k.to_string(), $crate::audio::util::Json::from($v))),*]) };
}
pub(crate) use obj;

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn num(&self) -> Option<f64> {
        if let Json::Num(n) = self { Some(*n) } else { None }
    }
    pub fn str(&self) -> Option<&str> {
        if let Json::Str(s) = self { Some(s) } else { None }
    }
    pub fn push(&mut self, key: &str, value: impl Into<Json>) {
        if let Json::Obj(items) = self {
            items.push((key.into(), value.into()));
        }
    }

    pub fn dump(&self, indent: usize) -> String {
        let mut s = String::new();
        self.write(&mut s, indent, 0);
        s
    }

    fn write(&self, s: &mut String, indent: usize, depth: usize) {
        let nl = |s: &mut String, d: usize| {
            if indent > 0 {
                s.push('\n');
                s.extend(std::iter::repeat_n(' ', indent * d));
            }
        };
        match self {
            Json::Null => s.push_str("null"),
            Json::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) if n.fract() == 0.0 && n.abs() < 9e15 => {
                let _ = write!(s, "{}", *n as i64);
            }
            Json::Num(n) => {
                let _ = write!(s, "{}", (n * 1e6).round() / 1e6);
            }
            Json::Str(v) => {
                s.push('"');
                for c in v.chars() {
                    match c {
                        '"' => s.push_str("\\\""),
                        '\\' => s.push_str("\\\\"),
                        '\n' => s.push_str("\\n"),
                        '\t' => s.push_str("\\t"),
                        c if (c as u32) < 0x20 => {
                            let _ = write!(s, "\\u{:04x}", c as u32);
                        }
                        c => s.push(c),
                    }
                }
                s.push('"');
            }
            Json::Arr(items) => {
                s.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    nl(s, depth + 1);
                    v.write(s, indent, depth + 1);
                }
                if !items.is_empty() {
                    nl(s, depth);
                }
                s.push(']');
            }
            Json::Obj(items) => {
                s.push('{');
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    nl(s, depth + 1);
                    Json::Str(k.clone()).write(s, indent, depth + 1);
                    s.push_str(if indent > 0 { ": " } else { ":" });
                    v.write(s, indent, depth + 1);
                }
                if !items.is_empty() {
                    nl(s, depth);
                }
                s.push('}');
            }
        }
    }

    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser { b: text.as_bytes(), i: 0 };
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("trailing data at byte {}", p.i));
        }
        Ok(v)
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("JSON: {what} at byte {}", self.i))
    }
    fn lit(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("bad literal")
        }
    }
    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.b.get(self.i) {
            Some(b'n') => self.lit("null", Json::Null),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(v));
                }
                loop {
                    v.push(self.value()?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(v));
                        }
                        _ => return self.err("expected , or ]"),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(v));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return self.err("expected :");
                    }
                    self.i += 1;
                    v.push((k, self.value()?));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(v));
                        }
                        _ => return self.err("expected , or }"),
                    }
                }
            }
            Some(_) => {
                let start = self.i;
                while self.b.get(self.i).is_some_and(|c| b"+-.eE0123456789".contains(c)) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[start..self.i]).ok().and_then(|s| s.parse().ok()).map(Json::Num).map_or_else(|| self.err("bad number"), Ok)
            }
            None => self.err("unexpected end"),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        if self.b.get(self.i) != Some(&b'"') {
            return self.err("expected string");
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            match self.b.get(self.i) {
                None => return self.err("unterminated string"),
                Some(b'"') => {
                    self.i += 1;
                    return String::from_utf8(out).map_err(|e| e.to_string());
                }
                Some(b'\\') => {
                    let c = *self.b.get(self.i + 1).ok_or("JSON: bad escape")?;
                    self.i += 2;
                    let ch = match c {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hex = std::str::from_utf8(self.b.get(self.i..self.i + 4).ok_or("JSON: bad \\u")?).map_err(|e| e.to_string())?;
                            self.i += 4;
                            char::from_u32(u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?).unwrap_or('\u{fffd}')
                        }
                        other => other as char,
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                Some(&c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }
}

/// Count occurrences by key, sorted.
pub fn counts<'a>(keys: impl Iterator<Item = &'a str>) -> BTreeMap<&'a str, usize> {
    let mut m = BTreeMap::new();
    for k in keys {
        *m.entry(k).or_insert(0) += 1;
    }
    m
}

pub fn human_size(v: u64) -> String {
    let mut size = v as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if size < 1024.0 || unit == "TiB" {
            return if unit == "B" { format!("{v} B") } else { format!("{size:.1} {unit}") };
        }
        size /= 1024.0;
    }
    format!("{v} B")
}
