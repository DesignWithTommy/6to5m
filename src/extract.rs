use crate::{
    scan::{File, Kind, Scan},
    zlib::Inflater,
};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

pub fn extract(scan: &Scan, report: &str, root: &Path) -> io::Result<()> {
    extract_only(scan, report, root, |_| true)
}

/// Like [`extract`], but only files whose type label (`Kind::label().0`)
/// passes `keep` are inflated, written and listed in index.tsv; report.txt
/// still describes the whole scan.
pub fn extract_only(scan: &Scan, report: &str, root: &Path, keep: impl Fn(&str) -> bool) -> io::Result<()> {
    if root.read_dir().is_ok_and(|mut entries| entries.next().is_some()) {
        let message = format!("{} is not empty", root.display());
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, message));
    }
    fs::create_dir_all(root)?;
    fs::write(root.join("report.txt"), report)?;

    let mut index = String::from("archive	offset	length	size	parts	type	kind	note
");
    let mut paths = Vec::with_capacity(scan.files.len());
    let mut chosen = Vec::with_capacity(scan.files.len());
    for file in &scan.files {
        let (at, archive) = (file.span.start, scan.owner(file.span.start));
        let (length, size, parts) = (file.span.len(), file.size(), file.parts.len());
        let (ext, name) = file.kind.label();
        if !keep(ext) {
            continue;
        }
        chosen.push(file);
        let note = if let Kind::Known(format) = &file.kind { &format.note } else { "" };
        index += &format!("{archive:#x}	{at:#x}	{length}	{size}	{parts}	{ext}	{name}	{note}
");
        paths.push(root.join(format!("h{archive:06x}/{at:08x}.{ext}")));
    }
    // Files are inflated and written in parallel; the first error in scan
    // order is reported, as the sequential loop did.
    let mut error = None;
    crate::par::ordered(
        chosen.len(),
        crate::par::threads() * 8,
        |i| {
            let path = &paths[i];
            write(scan.data, chosen[i], &mut Inflater::new(), path)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
        },
        |_, result| match result {
            Ok(()) => true,
            Err(e) => {
                error = Some(e);
                false
            }
        },
    );
    if let Some(e) = error {
        return Err(e);
    }
    fs::write(root.join("index.tsv"), index)
}

fn write(data: &[u8], file: &File, inflater: &mut Inflater, path: &Path) -> io::Result<()> {
    fs::create_dir_all(path.parent().unwrap_or(path))?;
    let mut out = fs::File::create_new(path)?;
    for part in &file.parts {
        let packed = &data[part.range.clone()];
        match part.inflated.and_then(|_| inflater.inflate(packed)) {
            Some(_) => out.write_all(&inflater.out)?,
            None => out.write_all(packed)?,
        }
    }
    Ok(())
}

/// Write one carved file's content (parts inflated) to a new file at `path`.
pub fn write_file(data: &[u8], file: &File, path: &Path) -> io::Result<()> {
    write(data, file, &mut Inflater::new(), path)
}

/// Up to `limit` bytes of a carved file's content, for previews.
pub fn head(data: &[u8], file: &File, limit: usize) -> Vec<u8> {
    let mut inflater = Inflater::new();
    let mut out = Vec::new();
    for part in &file.parts {
        if out.len() >= limit {
            break;
        }
        let packed = &data[part.range.clone()];
        match part.inflated.and_then(|_| inflater.inflate(packed)) {
            Some(_) => out.extend_from_slice(&inflater.out),
            None => out.extend_from_slice(&packed[..packed.len().min(limit)]),
        }
    }
    out.truncate(limit);
    out
}
