//! On-disk record of a model walk: texture-name lists (in walk order) and the
//! offsets of model resources. Models are re-decoded from their offsets on
//! replay, which skips probing every page of the archive.

pub enum Hit {
    Names(Vec<(u32, String)>),
    Model(usize),
}

pub fn store(head: &[u8], hits: Vec<Hit>) -> Vec<u8> {
    let mut b = head.to_vec();
    let put = |b: &mut Vec<u8>, v: u64| b.extend_from_slice(&v.to_le_bytes());
    put(&mut b, hits.len() as u64);
    for hit in hits {
        match hit {
            Hit::Model(at) => {
                b.push(0);
                put(&mut b, at as u64);
            }
            Hit::Names(list) => {
                b.push(1);
                put(&mut b, list.len() as u64);
                for (hash, name) in list {
                    b.extend_from_slice(&hash.to_le_bytes());
                    put(&mut b, name.len() as u64);
                    b.extend_from_slice(name.as_bytes());
                }
            }
        }
    }
    b
}

pub fn load(bytes: &[u8], head: &[u8], limit: usize) -> Option<Vec<Hit>> {
    parse(bytes.strip_prefix(head)?, limit)
}

fn parse(mut b: &[u8], limit: usize) -> Option<Vec<Hit>> {
    fn take<'a>(b: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let (h, r) = b.split_at_checked(n)?;
        *b = r;
        Some(h)
    }
    fn num(b: &mut &[u8]) -> Option<usize> {
        usize::try_from(u64::from_le_bytes(take(b, 8)?.try_into().ok()?)).ok()
    }
    let n = num(&mut b).filter(|&n| n <= limit)?;
    let mut hits = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        match take(&mut b, 1)?[0] {
            0 => hits.push(Hit::Model(num(&mut b).filter(|&at| at < limit)?)),
            1 => {
                let count = num(&mut b).filter(|&c| c <= limit)?;
                let mut list = Vec::with_capacity(count.min(1 << 16));
                for _ in 0..count {
                    let hash = u32::from_le_bytes(take(&mut b, 4)?.try_into().ok()?);
                    let len = num(&mut b).filter(|&l| l <= 1 << 16)?;
                    list.push((hash, String::from_utf8(take(&mut b, len)?.to_vec()).ok()?));
                }
                hits.push(Hit::Names(list));
            }
            _ => return None,
        }
    }
    b.is_empty().then_some(hits)
}
