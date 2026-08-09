//! Pure-Rust text extraction for plain-text + Office documents (txt, md,
//! docx, pptx, xlsx) and — when the `pdf-analyze` feature is on — text-layer
//! PDFs via pdfium. Image-only PDFs (no text layer) continue to flow through
//! the existing `shell::ocr` path for OCR.
#![allow(dead_code)] // wired into run_decoder_thread for FileKind::Doc / FileKind::Pdf.

use std::io::{Cursor, Read, Seek};
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

/// C1: cap on the number of glob-matched members iterated (e.g. `ppt/slides/`
/// slide parts). A crafted .pptx can list millions of `slideN.xml` entries; even
/// with the per-member byte cap, sorting + parsing every one of them burns
/// unbounded CPU on a decoder thread. No real presentation has tens of thousands
/// of slides, and `out` saturates at MAX_TEXT_BYTES long before then, so a bound
/// here is a pure DoS guard, never a correctness loss.
const MAX_GLOB_MEMBERS: usize = 50_000;

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
        "txt" | "md" => Some(read_plain(path, bytes)?),
        "docx" => Some(extract_zip_xml(path, bytes, &["word/document.xml"], &["w:t"])?),
        "pptx" => Some(extract_zip_xml_glob(path, bytes, "ppt/slides/slide", ".xml", &["a:t"])?),
        "xlsx" => Some(extract_zip_xml(path, bytes, &["xl/sharedStrings.xml"], &["t"])?),
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
    let mut zip =
        zip::ZipArchive::new(reader).with_context(|| format!("zip open {}", path.display()))?;
    let mut out = String::new();
    for member in members {
        let mut entry = match zip.by_name(member) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let xml = match read_member_bounded(&mut entry) {
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
fn read_member_bounded(entry: impl Read) -> Option<String> {
    let mut buf = Vec::new();
    if entry.take(MAX_MEMBER_BYTES).read_to_end(&mut buf).is_err() {
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

fn extract_zip_xml_glob_inner<R: Read + Seek>(
    reader: R,
    path: &Path,
    prefix: &str,
    suffix: &str,
    target_elems: &[&str],
) -> Result<String> {
    let mut zip =
        zip::ZipArchive::new(reader).with_context(|| format!("zip open {}", path.display()))?;
    // Collect only the glob-matched members, hard-capped at MAX_GLOB_MEMBERS so a
    // zip-bomb-shaped pptx (millions of slide entries) can't make the sort +
    // per-member parse below burn unbounded CPU.
    let mut names: Vec<String> = zip
        .file_names()
        .filter(|n| n.starts_with(prefix) && n.ends_with(suffix))
        .take(MAX_GLOB_MEMBERS)
        .map(String::from)
        .collect();
    names.sort();
    let mut out = String::new();
    for name in &names {
        let mut entry = match zip.by_name(name) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let xml = match read_member_bounded(&mut entry) {
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
                if let Ok(s) = t.unescape() {
                    out.push_str(&s);
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
    fn pptx_glob_member_iteration_is_capped() {
        // A zip-bomb-shaped pptx: 2× MAX_GLOB_MEMBERS slide members, each emitting
        // a single space (one <a:t> </a:t> run → one ' ' + a '\n' member separator
        // ≈ 2 bytes). 100 K members ≈ 200 KB stays UNDER MAX_TEXT_BYTES (256 KB),
        // so the byte cap never fires and can't mask the count cap. Without the
        // MAX_GLOB_MEMBERS `.take()`, all 100 K members are parsed; with it, at
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
