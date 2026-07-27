//! Pure-Rust text extraction for plain-text + Office documents (txt, md,
//! docx, pptx, xlsx) and — when the `pdf-analyze` feature is on — text-layer
//! PDFs via pdfium. Image-only PDFs (no text layer) continue to flow through
//! the existing `shell::ocr` path for OCR.
#![allow(dead_code)] // wired into run_decoder_thread for FileKind::Doc / FileKind::Pdf.

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};

/// Soft cap on text extracted per file. 256 KB is plenty for keyword/NER
/// extraction + a useful FTS5 snippet without bloating the DB on huge docs.
pub(crate) const MAX_TEXT_BYTES: usize = 256 * 1024;

/// C1: hard cap on the RAW bytes decompressed from any single zip member
/// (Office files are zips). Generous enough for legitimate XML markup overhead
/// (a 256 KB-text slide carries far less than this), but bounds a zip bomb —
/// a member that decompresses to gigabytes must never be fully materialized.
/// Enforced both via the member's declared uncompressed size AND a hard
/// `take()` on the reader (defends against a lying header).
const MAX_MEMBER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ARCHIVE_DECOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;

/// C1: cap on the number of glob-matched members iterated (e.g. `ppt/slides/`
/// slide parts). A crafted .pptx can list millions of `slideN.xml` entries; even
/// with the per-member byte cap, sorting + parsing every one of them burns
/// unbounded CPU on a decoder thread. No real presentation has tens of thousands
/// of slides, and `out` saturates at MAX_TEXT_BYTES long before then, so a bound
/// here is a pure DoS guard, never a correctness loss.
const MAX_GLOB_MEMBERS: usize = 5_000;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 32 * 1024 * 1024;

/// Extract text from `path` based on extension. Returns `Ok(None)` when the
/// extension is recognised-as-doc-but-unsupported (e.g. `.doc` legacy OLE)
/// AND when the extension isn't a document at all — callers treat both as
/// "no doc text" without distinguishing.
///
/// `bytes` is an optional pre-read content buffer (decoder thread reads the
/// file once for hashing + extraction on small files). When supplied, the
/// zip / text path skips a second file open; when `None`, the path-based
/// reader is used. PDF always uses the path because pdfium owns the file
/// handle and typical PDFs blow past the pre-read size cap.
pub(crate) fn extract(path: &Path, bytes: Option<&[u8]>) -> Result<Option<String>> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let text = match ext.as_str() {
        // Plain text: notes + source code + prose markup (lockstep with macOS FileTypes.code).
        "txt" | "md" | "swift" | "py" | "rb" | "js" | "jsx" | "ts" | "tsx" | "java" | "kt"
        | "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "cs" | "go" | "rs" | "php"
        | "sh" | "bash" | "zsh" | "sql" | "scala" | "m" | "mm" | "r" | "jl" | "lua" | "dart"
        | "vue" | "pl" | "pm" | "ps1" | "tex" | "bib" | "rst" | "org" | "adoc" => {
            Some(read_plain(path, bytes)?)
        }
        "docx" => Some(extract_zip_xml(path, bytes, &["word/document.xml"], &["w:t"])?),
        // OpenDocument Text: content.xml, paragraph/heading/span runs (text:p / text:h /
        // text:span). Near-identical to the macOS textutil odt path (same words).
        "odt" => Some(extract_zip_xml(path, bytes, &["content.xml"], &["p", "h", "span"])?),
        "pptx" => Some(extract_zip_xml_glob(path, bytes, "ppt/slides/slide", ".xml", &["a:t"])?),
        "xlsx" => Some(extract_zip_xml(path, bytes, &["xl/sharedStrings.xml"], &["t"])?),
        "epub" => Some(extract_epub(path, bytes)?),
        #[cfg(feature = "pdf-analyze")]
        "pdf" => extract_pdf_text(path).ok(),
        _ => None,
    };
    Ok(text.map(truncate_to_max))
}

fn truncate_to_max(mut t: String) -> String {
    if t.len() > MAX_TEXT_BYTES {
        // String::truncate panics off a char boundary, so clamp the cut point
        // down to one BEFORE truncating (floor_char_boundary isn't stable yet).
        let mut n = MAX_TEXT_BYTES;
        while !t.is_char_boundary(n) {
            n -= 1;
        }
        t.truncate(n);
    }
    t
}

fn read_plain(path: &Path, bytes: Option<&[u8]>) -> Result<String> {
    if let Some(b) = bytes {
        // Lossy decode keeps the existing semantics — `read_to_string` would
        // reject invalid UTF-8 with an error, and a single bad byte in a 1 MB
        // text file shouldn't sink the whole extraction. The keyword extractor
        // and FTS5 snippets can handle U+FFFD replacement chars fine.
        return Ok(String::from_utf8_lossy(b).into_owned());
    }
    // C4: read at most MAX_TEXT_BYTES (+ a small margin) instead of slurping
    // the whole file and truncating afterward — a multi-GB .txt/.md must not
    // be fully materialized just to keep 256 KB. Lossy decode matches the
    // bytes=Some path above.
    let p = crate::util::path_safety::to_extended_length(path);
    let file = std::fs::File::open(&p).with_context(|| format!("open text {}", p.display()))?;
    let mut buf = Vec::with_capacity(MAX_TEXT_BYTES.min(64 * 1024));
    file.take(MAX_TEXT_BYTES as u64 + 4)
        .read_to_end(&mut buf)
        .with_context(|| format!("read text {}", p.display()))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// PDF text extraction via the same `pdfium-render` binding `deep_analyze`
/// uses for rasterization. Pages are concatenated with newlines; image-only
/// PDFs (no text layer) return Ok("") — the OCR path picks them up.
#[cfg(feature = "pdf-analyze")]
fn extract_pdf_text(path: &Path) -> Result<String> {
    use pdfium_render::prelude::Pdfium;

    let p = crate::util::path_safety::to_extended_length(path);
    // Pdfium::default() unwraps the bind result and panics on a missing
    // pdfium.dll — taking the entire engine down per OS LoadLibrary error
    // 126. Bind explicitly so a missing/broken DLL becomes a per-file Err
    // that the caller (extract() above) silently turns into "no PDF text".
    let bindings = Pdfium::bind_to_system_library()
        .map_err(|e| anyhow::anyhow!("pdfium bind: {e}"))?;
    let pdfium = Pdfium::new(bindings);
    let doc = pdfium
        .load_pdf_from_file(&p, None)
        .with_context(|| format!("pdfium load {}", path.display()))?;
    let mut out = String::new();
    for page in doc.pages().iter() {
        let Ok(text) = page.text() else { continue };
        let s = text.all();
        if !out.is_empty() && !s.is_empty() {
            out.push('\n');
        }
        out.push_str(&s);
        if out.len() > MAX_TEXT_BYTES {
            break;
        }
    }
    Ok(out)
}

/// Pull text out of named members in a zip archive.
fn extract_zip_xml(
    path: &Path,
    bytes: Option<&[u8]>,
    members: &[&str],
    target_elems: &[&str],
) -> Result<String> {
    if let Some(b) = bytes {
        extract_zip_xml_inner(Cursor::new(b), path, members, target_elems)
    } else {
        let p = crate::util::path_safety::to_extended_length(path);
        let file = std::fs::File::open(&p)?;
        extract_zip_xml_inner(file, path, members, target_elems)
    }
}

fn extract_zip_xml_inner<R: Read + Seek>(
    reader: R,
    path: &Path,
    members: &[&str],
    target_elems: &[&str],
) -> Result<String> {
    let mut zip = open_bounded_zip(reader, path)?;
    let mut out = String::new();
    let mut remaining_decompressed = MAX_ARCHIVE_DECOMPRESSED_BYTES;
    for member in members {
        let mut entry = match zip.by_name(member) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let xml = match read_member_bounded(&mut entry, &mut remaining_decompressed) {
            Some(x) => x,
            None => continue,
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&xml_text_runs(&xml, target_elems));
        if out.len() > MAX_TEXT_BYTES {
            break;
        }
    }
    Ok(out)
}

/// Read a single zip member into a String, hard-bounded by `MAX_MEMBER_BYTES`
/// so a zip bomb (a member that decompresses to gigabytes) can never be fully
/// materialized — `take()` stops after the cap of DECOMPRESSED output. Lossy
/// UTF-8 (the bytes=Some / read_plain paths already are).
fn read_member_bounded(entry: impl Read, remaining_decompressed: &mut u64) -> Option<String> {
    let allowed = MAX_MEMBER_BYTES.min(*remaining_decompressed);
    if allowed == 0 {
        return None;
    }
    let mut buf = Vec::new();
    if entry
        .take(allowed.saturating_add(1))
        .read_to_end(&mut buf)
        .is_err()
    {
        return None;
    }
    *remaining_decompressed = (*remaining_decompressed).saturating_sub(buf.len() as u64);
    if buf.len() as u64 > allowed {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Pull text out of every member whose name starts with `prefix` and ends
/// with `suffix` (e.g. `ppt/slides/slide` + `.xml` for PowerPoint). Members
/// are visited in sorted (slide) order.
fn extract_zip_xml_glob(
    path: &Path,
    bytes: Option<&[u8]>,
    prefix: &str,
    suffix: &str,
    target_elems: &[&str],
) -> Result<String> {
    if let Some(b) = bytes {
        extract_zip_xml_glob_inner(Cursor::new(b), path, prefix, suffix, target_elems)
    } else {
        let p = crate::util::path_safety::to_extended_length(path);
        let file = std::fs::File::open(&p)?;
        extract_zip_xml_glob_inner(file, path, prefix, suffix, target_elems)
    }
}

fn open_bounded_zip<R: Read + Seek>(mut reader: R, path: &Path) -> Result<zip::ZipArchive<R>> {
    let end = reader.seek(SeekFrom::End(0))?;
    let tail_len = end.min(65_557) as usize;
    reader.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0u8; tail_len];
    reader.read_exact(&mut tail)?;
    let eocd = tail
        .windows(4)
        .enumerate()
        .rev()
        .find_map(|(offset, window)| {
            if window != b"PK\x05\x06" || offset + 22 > tail.len() {
                return None;
            }
            let comment_len =
                u16::from_le_bytes([tail[offset + 20], tail[offset + 21]]) as usize;
            (offset + 22 + comment_len == tail.len()).then_some(offset)
        })
        .with_context(|| format!("zip end-of-central-directory record missing: {}", path.display()))?;
    let disk = u16::from_le_bytes([tail[eocd + 4], tail[eocd + 5]]);
    let central_disk = u16::from_le_bytes([tail[eocd + 6], tail[eocd + 7]]);
    let disk_entries = u16::from_le_bytes([tail[eocd + 8], tail[eocd + 9]]) as usize;
    let entries = u16::from_le_bytes([tail[eocd + 10], tail[eocd + 11]]) as usize;
    let central_bytes_u32 = u32::from_le_bytes([
        tail[eocd + 12],
        tail[eocd + 13],
        tail[eocd + 14],
        tail[eocd + 15],
    ]);
    let central_offset_u32 = u32::from_le_bytes([
        tail[eocd + 16],
        tail[eocd + 17],
        tail[eocd + 18],
        tail[eocd + 19],
    ]);
    anyhow::ensure!(
        disk == 0 && central_disk == 0 && disk_entries == entries,
        "multi-disk document archives are not accepted for text extraction"
    );
    anyhow::ensure!(
        entries != u16::MAX as usize
            && central_bytes_u32 != u32::MAX
            && central_offset_u32 != u32::MAX,
        "ZIP64 document archives are not accepted for text extraction"
    );
    anyhow::ensure!(
        entries <= MAX_ARCHIVE_ENTRIES,
        "document archive has {entries} entries; limit is {MAX_ARCHIVE_ENTRIES}"
    );
    let central_bytes = u64::from(central_bytes_u32);
    let central_offset = u64::from(central_offset_u32);
    anyhow::ensure!(
        central_bytes <= MAX_CENTRAL_DIRECTORY_BYTES,
        "document archive central directory exceeds {} bytes",
        MAX_CENTRAL_DIRECTORY_BYTES
    );
    let eocd_absolute = end - tail_len as u64 + eocd as u64;
    anyhow::ensure!(
        central_offset.checked_add(central_bytes) == Some(eocd_absolute),
        "document archive central directory layout is inconsistent"
    );
    if entries > 0 {
        reader.seek(SeekFrom::Start(central_offset))?;
        let mut signature = [0u8; 4];
        reader.read_exact(&mut signature)?;
        anyhow::ensure!(
            &signature == b"PK\x01\x02",
            "document archive central directory signature is invalid"
        );
    }
    reader.seek(SeekFrom::Start(0))?;
    zip::ZipArchive::new(reader).with_context(|| format!("zip open {}", path.display()))
}

fn extract_zip_xml_glob_inner<R: Read + Seek>(
    reader: R,
    path: &Path,
    prefix: &str,
    suffix: &str,
    target_elems: &[&str],
) -> Result<String> {
    let mut zip = open_bounded_zip(reader, path)?;
    // Collect only the glob-matched members, hard-capped at MAX_GLOB_MEMBERS so a
    // zip-bomb-shaped pptx (millions of slide entries) can't make the sort +
    // per-member parse below burn unbounded CPU.
    let mut names: Vec<String> = zip
        .file_names()
        .take(MAX_ARCHIVE_ENTRIES)
        .filter(|n| n.starts_with(prefix) && n.ends_with(suffix))
        .take(MAX_GLOB_MEMBERS)
        .map(String::from)
        .collect();
    names.sort();
    let mut out = String::new();
    let mut remaining_decompressed = MAX_ARCHIVE_DECOMPRESSED_BYTES;
    for name in &names {
        let mut entry = match zip.by_name(name) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let xml = match read_member_bounded(&mut entry, &mut remaining_decompressed) {
            Some(x) => x,
            None => continue,
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&xml_text_runs(&xml, target_elems));
        if out.len() > MAX_TEXT_BYTES {
            break;
        }
    }
    Ok(out)
}

/// EPUB → text. An EPUB is a zip of XHTML; concatenate the tag-stripped text of its
/// (x)html members in reading (sorted) order, bounded the same way as the OOXML globs.
/// Mirrors the macOS `DocText.epubText`.
fn extract_epub(path: &Path, bytes: Option<&[u8]>) -> Result<String> {
    if let Some(b) = bytes {
        extract_epub_inner(Cursor::new(b), path)
    } else {
        let p = crate::util::path_safety::to_extended_length(path);
        let file = std::fs::File::open(&p)?;
        extract_epub_inner(file, path)
    }
}

fn extract_epub_inner<R: Read + Seek>(reader: R, path: &Path) -> Result<String> {
    let mut zip = open_bounded_zip(reader, path)?;
    let mut names: Vec<String> = zip
        .file_names()
        .take(MAX_ARCHIVE_ENTRIES)
        .filter(|n| {
            std::path::Path::new(n)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| {
                    e.eq_ignore_ascii_case("xhtml")
                        || e.eq_ignore_ascii_case("html")
                        || e.eq_ignore_ascii_case("htm")
                })
        })
        .take(MAX_GLOB_MEMBERS)
        .map(String::from)
        .collect();
    names.sort();
    let mut out = String::new();
    let mut remaining_decompressed = MAX_ARCHIVE_DECOMPRESSED_BYTES;
    for name in &names {
        let mut entry = match zip.by_name(name) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let html = match read_member_bounded(&mut entry, &mut remaining_decompressed) {
            Some(h) => h,
            None => continue,
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&strip_tags(&html));
        if out.len() > MAX_TEXT_BYTES {
            break;
        }
    }
    Ok(out)
}

/// Strip XML/HTML tags (each becomes a space) and collapse whitespace — cheap, dep-free,
/// good enough for a BGE clustering snippet. Leaves entities (`&amp;`) as-is, matching the
/// macOS `epubText` regex strip.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut last_ws = true; // suppress leading + run whitespace
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            // Each closed tag becomes a space (then whitespace collapses) — matches the
            // macOS `epubText` regex replace, so the extracted text is the same.
            '>' => {
                in_tag = false;
                if !last_ws {
                    out.push(' ');
                    last_ws = true;
                }
            }
            _ if in_tag => {}
            c if c.is_whitespace() => {
                if !last_ws {
                    out.push(' ');
                    last_ws = true;
                }
            }
            c => {
                out.push(c);
                last_ws = false;
            }
        }
    }
    out.trim().to_string()
}

/// Walk `xml` with quick-xml's pull parser, accumulating text from every
/// element whose **local name** appears in `target_elems` (the namespace
/// prefix before `:` is ignored). `["w:t", "t"]` matches `<w:t>`, `<a:t>`,
/// and bare `<t>` alike.
fn xml_text_runs(xml: &str, target_elems: &[&str]) -> String {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let matches = |name: &str| -> bool {
        let local = name.rsplit_once(':').map_or(name, |(_, l)| l);
        target_elems.iter().any(|t| {
            let t_local = t.rsplit_once(':').map_or(*t, |(_, l)| l);
            local == t_local
        })
    };

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out = String::new();
    let mut depth: u32 = 0;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if matches(&name) {
                    depth += 1;
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if matches(&name) && depth > 0 {
                    depth -= 1;
                    out.push(' ');
                }
            }
            Ok(Event::Text(t)) if depth > 0 => {
                // quick-xml 0.41 emits entity refs as separate `GeneralRef`
                // events, so `Text` payloads are already entity-free — a plain
                // charset decode restores the run faithfully.
                if let Ok(s) = t.decode() {
                    out.push_str(&s);
                }
            }
            Ok(Event::GeneralRef(r)) if depth > 0 => {
                // The `&amp;`/`&#8217;` runs the old `unescape()` used to fold
                // into `Text`. Resolve numeric char refs and the five
                // predefined named entities; drop unknown custom entities
                // (the old `unescape()` errored on those and we skipped too).
                if let Ok(Some(ch)) = r.resolve_char_ref() {
                    out.push(ch);
                } else if let Ok(name) = r.decode() {
                    if let Some(rep) = quick_xml::escape::resolve_predefined_entity(&name) {
                        out.push_str(rep);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        if out.len() > MAX_TEXT_BYTES {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn strip_tags_keeps_text_drops_markup() {
        let html = "<html><body><h1>Moby Dick</h1>\n<p>Call me  Ishmael.</p></body></html>";
        assert_eq!(strip_tags(html), "Moby Dick Call me Ishmael.");
        assert_eq!(strip_tags("<p>a</p><p>b</p>"), "a b");
        assert_eq!(strip_tags("   <i></i>   "), "");
    }

    fn tmp_with(suffix: &str, body: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "fileid-doc-{}-{}{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            suffix
        ));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn extract_text_file_passes_through() {
        let p = tmp_with(".txt", b"hello world");
        let t = extract(&p, None).unwrap().unwrap();
        assert_eq!(t, "hello world");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn extract_markdown_keeps_words() {
        let p = tmp_with(".md", b"# Heading\n\nBody text with **bold** parts.");
        let t = extract(&p, None).unwrap().unwrap();
        assert!(t.contains("Body"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn extract_unsupported_extension_yields_none() {
        let p = tmp_with(".jpg", b"fake");
        let t = extract(&p, None).unwrap();
        assert!(t.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn extract_text_bytes_equivalent_to_path() {
        let body = b"hello bytes path equivalence";
        let p = tmp_with(".txt", body);
        let via_path = extract(&p, None).unwrap().unwrap();
        let via_bytes = extract(&p, Some(body)).unwrap().unwrap();
        assert_eq!(via_path, via_bytes);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn extract_zip_bytes_equivalent_to_path() {
        // Minimal docx-shaped zip in memory: one entry word/document.xml with a
        // <w:t> run. Skip the test if zip writing fails (extreme env weirdness).
        let mut buf = Vec::new();
        {
            use std::io::Write;
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            zw.start_file::<_, ()>(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zw.write_all(b"<root><w:t>hello docx</w:t></root>").unwrap();
            zw.finish().unwrap();
        }
        let p = tmp_with(".docx", &buf);
        let via_path = extract(&p, None).unwrap().unwrap();
        let via_bytes = extract(&p, Some(&buf)).unwrap().unwrap();
        assert_eq!(via_path, via_bytes);
        assert!(via_bytes.contains("hello docx"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn xml_text_runs_collects_only_target_elements() {
        let xml = r"<root><w:t>hello </w:t><meta>skip</meta><w:t>world</w:t></root>";
        let out = xml_text_runs(xml, &["w:t"]);
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
        assert!(!out.contains("skip"));
    }

    #[test]
    fn xml_text_runs_matches_local_name_across_namespaces() {
        let xml = r#"<a:p xmlns:a="x"><a:t>aa</a:t><b:t xmlns:b="y">bb</b:t></a:p>"#;
        let out = xml_text_runs(xml, &["t"]);
        assert!(out.contains("aa"));
        assert!(out.contains("bb"));
    }

    #[test]
    fn xml_text_runs_resolves_entities() {
        // Guards the quick-xml 0.41 migration: text runs must still be
        // entity-decoded (predefined + numeric), not left as raw `&amp;`.
        let xml = r"<root><w:t>Tom &amp; Jerry &#8217;s day &lt;3</w:t></root>";
        let out = xml_text_runs(xml, &["w:t"]);
        assert_eq!(out.trim(), "Tom & Jerry \u{2019}s day <3");
    }

    #[test]
    fn pptx_glob_member_iteration_is_capped() {
        // A zip-bomb-shaped pptx: 2× MAX_GLOB_MEMBERS slide members, each emitting
        // a single space (one <a:t> </a:t> run → one ' ' + a '\n' member separator
        // ≈ 2 bytes). The matched-member set stays UNDER MAX_TEXT_BYTES, so the
        // byte cap never fires and can't mask the count cap. Without the
        // MAX_GLOB_MEMBERS `.take()`, all members are parsed; with it, at
        // most MAX_GLOB_MEMBERS are. We count emitted member segments (newline
        // separators) and assert the bound — order-independent, so it doesn't rely
        // on zip iteration order.
        const OVER_CAP: usize = MAX_GLOB_MEMBERS * 2;
        let mut buf = Vec::new();
        {
            use std::io::Write;
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            // Stored (no deflate) keeps writing 100 K tiny members fast.
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for i in 0..OVER_CAP {
                zw.start_file::<_, ()>(format!("ppt/slides/slide{i:07}.xml"), opts)
                    .unwrap();
                // A single empty text run → exactly one space of output per member.
                zw.write_all(b"<p><a:t> </a:t></p>").unwrap();
            }
            zw.finish().unwrap();
        }
        let path = std::path::Path::new("bomb.pptx");
        let out = extract_zip_xml_glob_inner(
            Cursor::new(&buf),
            path,
            "ppt/slides/slide",
            ".xml",
            &["a:t"],
        )
        .unwrap();
        // One '\n' is inserted between members, so processed-member count is
        // (newlines + 1). Bounded by the cap rather than the full OVER_CAP.
        let processed = out.matches('\n').count() + 1;
        assert!(
            processed <= MAX_GLOB_MEMBERS,
            "member iteration must stop at MAX_GLOB_MEMBERS, processed {processed}"
        );
        assert!(out.len() < MAX_TEXT_BYTES, "byte cap must not have masked the count cap");
    }

    #[test]
    fn archive_entry_preflight_rejects_central_directory_amplification() {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for i in 0..=MAX_ARCHIVE_ENTRIES {
                zw.start_file::<_, ()>(format!("noise/{i:07}.bin"), opts).unwrap();
            }
            zw.finish().unwrap();
        }
        let err = match open_bounded_zip(Cursor::new(&buf), Path::new("too-many.docx")) {
            Ok(_) => panic!("entry count over the cap must fail before ZipArchive parses it"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("entries"));
    }

    #[test]
    fn archive_preflight_rejects_fake_eocd_inside_the_real_comment() {
        let mut buf = Vec::new();
        {
            use std::io::Write;
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            zw.start_file::<_, ()>(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zw.write_all(b"<w:t>safe</w:t>").unwrap();
            zw.finish().unwrap();
        }
        let real_eocd = buf
            .windows(4)
            .rposition(|window| window == b"PK\x05\x06")
            .unwrap();
        buf[real_eocd + 20..real_eocd + 22].copy_from_slice(&22u16.to_le_bytes());
        let mut fake_eocd = [0u8; 22];
        fake_eocd[..4].copy_from_slice(b"PK\x05\x06");
        buf.extend_from_slice(&fake_eocd);

        let err = match open_bounded_zip(Cursor::new(&buf), Path::new("fake-comment.docx")) {
            Ok(_) => panic!("a fake EOCD in the real comment must not bypass preflight"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("layout"));
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let s = "é".repeat(200_000);
        let truncated = truncate_to_max(s);
        assert!(truncated.len() <= MAX_TEXT_BYTES);
        // Roundtrip valid UTF-8 (every char must still be 'é').
        assert!(truncated.chars().all(|c| c == 'é'));
    }

    #[test]
    fn truncate_handles_cut_inside_multibyte_char() {
        // 3-byte chars: MAX_TEXT_BYTES (262144) % 3 == 1, so the byte cut
        // always lands mid-char — the case that used to panic in
        // String::truncate before the boundary clamp.
        let s = "夏".repeat(100_000);
        let truncated = truncate_to_max(s);
        assert!(truncated.len() <= MAX_TEXT_BYTES);
        assert!(truncated.len() > MAX_TEXT_BYTES - 4);
        assert!(truncated.chars().all(|c| c == '夏'));
    }
}
