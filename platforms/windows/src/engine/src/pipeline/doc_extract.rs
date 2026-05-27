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
        t.truncate(MAX_TEXT_BYTES);
        while !t.is_char_boundary(t.len()) {
            t.pop();
        }
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
    let p = crate::util::path_safety::to_extended_length(path);
    std::fs::read_to_string(&p).with_context(|| format!("read text {}", p.display()))
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
        let mut xml = String::new();
        let _ = entry.read_to_string(&mut xml);
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
    let mut names: Vec<String> = zip.file_names().map(String::from).collect();
    names.sort();
    let mut out = String::new();
    for name in names.iter().filter(|n| n.starts_with(prefix) && n.ends_with(suffix)) {
        let mut entry = match zip.by_name(name) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let mut xml = String::new();
        let _ = entry.read_to_string(&mut xml);
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
    fn truncate_respects_char_boundary() {
        let s = "é".repeat(200_000);
        let truncated = truncate_to_max(s);
        assert!(truncated.len() <= MAX_TEXT_BYTES);
        // Roundtrip valid UTF-8 (every char must still be 'é').
        assert!(truncated.chars().all(|c| c == 'é'));
    }
}
