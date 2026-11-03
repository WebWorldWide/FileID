//! Randomized decoder fuzzer. Mutates seed files (synthetic images, RAW
//! containers, office/EPUB zips, audio tags, OBJ text) and feeds them to every
//! parser that reads untrusted bytes. Asserts no panic on the fuzz thread
//! (caught or not), a time bound per call, and decoded frames that respect the
//! pixel cap. A long run (`FILEID_FUZZ_DECODER_ITERATIONS`) also seeds with
//! the first 4 MiB of real camera files from `FILEID_FUZZ_CORPUS`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::server::fuzz_tests::{iterations, record_panics, Rng};

#[derive(Clone, Copy, Debug)]
enum Kind {
    Image(&'static str),
    Raw(&'static str),
    Doc(&'static str),
    Audio(&'static str),
    Obj,
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(width, height, |x, y| image::Rgb([(x * 7) as u8, (y * 5) as u8, ((x ^ y) * 3) as u8]));
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80).encode_image(&img).unwrap();
    out
}

fn encoded(format: image::ImageFormat) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(40, 30, |x, y| image::Rgb([x as u8, y as u8, 99])));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, format).unwrap();
    out.into_inner()
}

/// Little-endian TIFF with IFD0 (orientation, GPS IFD, SubIFD) and an
/// embedded JPEG preview, the shape of most camera RAWs.
fn tiff_raw(preview: &[u8]) -> Vec<u8> {
    let mut out = b"II*\0".to_vec();
    out.extend_from_slice(&8u32.to_le_bytes());
    let entries: [(u16, u16, u32, u32); 6] = [
        (0x0112, 3, 1, 6),
        (0x0103, 3, 1, 6),
        (0x0201, 4, 1, 200),
        (0x0202, 4, 1, preview.len() as u32),
        (0x8825, 4, 1, 120),
        (0x014A, 4, 1, 150),
    ];
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, kind, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.resize(120, 0);
    // GPS IFD: latitude ref + rational latitude with a zero denominator.
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&[1, 0, 2, 0, 2, 0, 0, 0, b'N', 0, 0, 0]);
    out.extend_from_slice(&[2, 0, 5, 0, 3, 0, 0, 0, 140, 0, 0, 0]);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.resize(140, 0);
    out.extend_from_slice(&[10, 0, 0, 0, 0, 0, 0, 0, 30, 0, 0, 0, 1, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0]);
    out.resize(150, 0);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.resize(200, 0);
    out.extend_from_slice(preview);
    out
}

fn raf(preview: &[u8]) -> Vec<u8> {
    let mut out = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
    out.resize(84, 0);
    out.extend_from_slice(&100u32.to_be_bytes());
    out.extend_from_slice(&(preview.len() as u32).to_be_bytes());
    out.resize(100, 0);
    out.extend_from_slice(preview);
    out
}

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn cr3(preview: &[u8]) -> Vec<u8> {
    let tiff = &tiff_raw(&[])[..150];
    let mut meta = vec![0u8; 16];
    meta.extend(bx(b"CMT1", tiff));
    let mut prvw = vec![0u8; 24];
    prvw.extend(bx(b"PRVW", &[vec![0u8; 8], preview.to_vec()].concat()));
    [bx(b"ftyp", b"crx \0\0\0\x01"), bx(b"moov", &bx(b"uuid", &meta)), bx(b"uuid", &prvw)].concat()
}

fn zip(entries: &[(&str, Vec<u8>)], deflate: bool) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let method = if deflate { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored };
        for (name, body) in entries {
            zw.start_file::<_, ()>(*name, zip::write::SimpleFileOptions::default().compression_method(method)).unwrap();
            zw.write_all(body).unwrap();
        }
        zw.finish().unwrap();
    }
    buf.into_inner()
}

fn id3_mp3() -> Vec<u8> {
    let title = b"\x03Fuzz Song";
    let mut frame = b"TIT2".to_vec();
    frame.extend_from_slice(&(title.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(title);
    let mut out = b"ID3\x03\0\0".to_vec();
    out.extend_from_slice(&[0, 0, 0, frame.len() as u8]);
    out.extend(frame);
    for _ in 0..8 {
        out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64]);
        out.extend(std::iter::repeat_n(0u8, 413));
    }
    out
}

fn wav() -> Vec<u8> {
    let info = [b"INFO".to_vec(), b"INAM".to_vec(), 6u32.to_le_bytes().to_vec(), b"Title\0".to_vec()].concat();
    let fmt = [1u16.to_le_bytes(), 1u16.to_le_bytes()].concat();
    let fmt = [fmt, 8000u32.to_le_bytes().to_vec(), 16000u32.to_le_bytes().to_vec(), 2u16.to_le_bytes().to_vec(), 16u16.to_le_bytes().to_vec()].concat();
    let chunks = [
        b"fmt ".to_vec(), (fmt.len() as u32).to_le_bytes().to_vec(), fmt,
        b"LIST".to_vec(), (info.len() as u32).to_le_bytes().to_vec(), info,
        b"data".to_vec(), 64u32.to_le_bytes().to_vec(), vec![0; 64],
    ]
    .concat();
    [b"RIFF".to_vec(), ((chunks.len() + 4) as u32).to_le_bytes().to_vec(), b"WAVE".to_vec(), chunks].concat()
}

fn m4a() -> Vec<u8> {
    let data = bx(b"data", b"\0\0\0\x01\0\0\0\0Fuzz Title");
    let ilst = bx(b"ilst", &bx(b"\xa9nam", &data));
    let meta = bx(b"meta", &[vec![0u8; 4], bx(b"hdlr", &[0u8; 25]), ilst].concat());
    let stco = bx(b"stco", &[0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1]);
    let trak = bx(b"trak", &bx(b"mdia", &[bx(b"hdlr", &[0u8; 25]), bx(b"minf", &bx(b"stbl", &stco))].concat()));
    [bx(b"ftyp", b"M4A \0\0\0\0"), bx(b"moov", &[trak, bx(b"udta", &meta)].concat())].concat()
}

fn seeds(heavy: bool) -> Vec<(Kind, Vec<u8>)> {
    let small = jpeg(64, 48);
    let big = jpeg(1200, 900);
    let mut progressive_bomb = vec![0xFF, 0xD8, 0xFF, 0xC2, 0, 17, 8, 0xFF, 0xFF, 0xFF, 0xFF, 3];
    for id in 1..=3 {
        progressive_bomb.extend_from_slice(&[id, 0x11, 0]);
    }
    progressive_bomb.extend_from_slice(&[0xFF, 0xDA, 0, 8, 1, 1, 0, 0, 0x3F, 0]);
    progressive_bomb.extend(std::iter::repeat_n(0u8, 64));
    let docx = zip(&[("word/document.xml", b"<w:document><w:body><w:p><w:r><w:t>hello fuzz docx</w:t></w:r></w:p></w:body></w:document>".to_vec())], true);
    let pptx = zip(&[("ppt/slides/slide1.xml", b"<p:sld><a:t>slide text</a:t></p:sld>".to_vec())], false);
    let xlsx = zip(&[("xl/sharedStrings.xml", b"<sst><si><t>cell</t></si></sst>".to_vec())], true);
    let epub = zip(&[("mimetype", b"application/epub+zip".to_vec()), ("OEBPS/ch1.xhtml", b"<html><body><p>chapter one</p></body></html>".to_vec())], false);
    let odt = zip(&[("content.xml", b"<office:text><text:p>odt words</text:p></office:text>".to_vec())], true);
    let bomb_size = if heavy { 40 } else { 17 } * 1024 * 1024;
    let bomb = zip(&[("word/document.xml", vec![b'a'; bomb_size])], true);
    let many: Vec<(String, Vec<u8>)> = (0..3_000).map(|i| (format!("ppt/slides/slide{i}.xml"), b"<a:t>x</a:t>".to_vec())).collect();
    let many_refs: Vec<(&str, Vec<u8>)> = many.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    let deep = format!("<w:document>{}<w:t>deep</w:t>{}</w:document>", "<w:p>".repeat(20_000), "</w:p>".repeat(20_000));
    let mut out = vec![
        (Kind::Image("jpg"), small.clone()),
        (Kind::Image("jpg"), big.clone()),
        (Kind::Image("jpg"), progressive_bomb),
        (Kind::Image("jpg"), tiff_raw(&small)[..200].to_vec().into_iter().chain(small.clone()).collect()),
        (Kind::Image("png"), encoded(image::ImageFormat::Png)),
        (Kind::Image("gif"), encoded(image::ImageFormat::Gif)),
        (Kind::Image("bmp"), encoded(image::ImageFormat::Bmp)),
        (Kind::Image("tif"), encoded(image::ImageFormat::Tiff)),
        (Kind::Raw("dng"), tiff_raw(&big)),
        (Kind::Raw("nef"), tiff_raw(&small)),
        (Kind::Raw("raf"), raf(&big)),
        (Kind::Raw("cr3"), cr3(&big)),
        (Kind::Doc("docx"), docx),
        (Kind::Doc("pptx"), pptx),
        (Kind::Doc("xlsx"), xlsx),
        (Kind::Doc("epub"), epub),
        (Kind::Doc("odt"), odt),
        (Kind::Doc("docx"), bomb),
        (Kind::Doc("pptx"), zip(&many_refs, false)),
        (Kind::Doc("docx"), zip(&[("word/document.xml", deep.into_bytes())], true)),
        (Kind::Doc("txt"), "plain words ".repeat(5000).into_bytes()),
        (Kind::Audio("mp3"), id3_mp3()),
        (Kind::Audio("wav"), wav()),
        (Kind::Audio("m4a"), m4a()),
        (Kind::Obj, b"mtllib x.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nf 1 2 3\nf 1 3 4\nf -1 -2 -3\nf 1/1/1 2//2 4\n".to_vec()),
    ];
    let Some(corpus) = std::env::var_os("FILEID_FUZZ_CORPUS").map(PathBuf::from).filter(|_| heavy) else {
        return out;
    };
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(&corpus)
        .max_depth(6)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    files.sort();
    let (mut jpgs, mut raws) = (0, 0);
    for path in files {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let kind = match ext.as_str() {
            "jpg" | "jpeg" if jpgs < 4 => {
                jpgs += 1;
                Kind::Image("jpg")
            }
            "arw" | "cr2" | "cr3" | "nef" | "dng" | "raf" | "orf" | "rw2" if raws < 3 => {
                raws += 1;
                Kind::Raw("arw")
            }
            _ => continue,
        };
        // A copy of the head is enough to reach every header and preview.
        let mut head = Vec::new();
        if let Ok(file) = std::fs::File::open(&path) {
            let _ = std::io::Read::read_to_end(&mut std::io::Read::take(file, 4 * 1024 * 1024), &mut head);
            out.push((kind, head));
        }
    }
    out
}

fn mutate(mut bytes: Vec<u8>, rng: &mut Rng) -> Vec<u8> {
    for _ in 0..=rng.below(6) {
        if bytes.is_empty() {
            bytes.push(0);
        }
        let at = rng.below(bytes.len());
        match rng.below(8) {
            0 => bytes[at] ^= 1 << rng.below(8),
            1 => bytes[at] = [0x00, 0xFF, 0x7F, 0x80, 0x01][rng.below(5)],
            2 => bytes.truncate(at),
            3 => {
                let value = [0u32, u32::MAX, 0x7FFF_FFFF, 0x8000_0000, 1, 0xFFFF][rng.below(6)];
                let end = (at + 4).min(bytes.len());
                let encoded = if rng.chance(50) { value.to_be_bytes() } else { value.to_le_bytes() };
                bytes[at..end].copy_from_slice(&encoded[..end - at]);
            }
            4 => {
                let end = (at + 1 + rng.below(512)).min(bytes.len());
                let chunk = bytes[at..end].to_vec();
                let to = rng.below(bytes.len());
                bytes.splice(to..to, chunk);
            }
            5 => {
                let end = (at + 1 + rng.below(4096)).min(bytes.len());
                bytes.drain(at..end);
            }
            6 => {
                let marker: &[u8] = [&b"\xFF\xD8\xFF"[..], b"\xFF\xC2", b"ftyp", b"moov", b"ID3", b"PK\x03\x04", b"\x1A\x45\xDF\xA3", b"caff"][rng.below(8)];
                bytes.splice(at..at, marker.iter().copied());
            }
            _ => {
                let len = rng.below(64);
                for b in bytes.iter_mut().skip(at).take(len) {
                    *b = rng.next() as u8;
                }
            }
        }
    }
    bytes
}

fn check_frame(frame: Option<&(Vec<u8>, u32, u32)>) -> Option<String> {
    let (rgb, w, h) = frame?;
    let pixels = u64::from(*w) * u64::from(*h);
    if pixels > super::tagging::MAX_DECODED_PIXELS {
        return Some(format!("decoded {w}x{h} over the pixel cap"));
    }
    if rgb.len() as u64 != pixels * 3 {
        return Some(format!("decoded {w}x{h} frame has {} bytes", rgb.len()));
    }
    None
}

fn run_one(kind: Kind, bytes: &[u8], scratch: &Path, problems: &mut Vec<String>) {
    use super::tagging as t;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(256 * 1024)]);
    match kind {
        Kind::Image(ext) => {
            let path = scratch.join(format!("fuzz.{ext}"));
            let frame = t::decode_image_sync(&path, Some(bytes)).ok();
            problems.extend(check_frame(frame.as_ref()));
            if let Some((rgb, w, h)) = &frame {
                let _ = t::compute_dhash(rgb, *w as usize, *h as usize);
            }
            problems.extend(check_frame(t::decode_jpeg_scaled_to(bytes, 256).as_ref()));
            let _ = t::parse_exif_fields(bytes);
            let _ = t::probe_image_decode_reservation_bytes(bytes);
            let _ = t::exif_orientation(bytes);
        }
        Kind::Raw(ext) => {
            let _ = super::raw_preview::largest_embedded_jpeg(bytes);
            let _ = super::raw_preview::orientation(bytes);
            let path = scratch.join(format!("fuzz.{ext}"));
            problems.extend(check_frame(t::decode_raw_preview_at_least(&path, Some(bytes), 64, 64).as_ref()));
            problems.extend(check_frame(t::decode_image_sync(&path, Some(bytes)).ok().as_ref()));
        }
        Kind::Doc(ext) => {
            if let Ok(Some(found)) = super::doc_extract::extract(&scratch.join(format!("fuzz.{ext}")), Some(bytes)) {
                let _ = crate::util::keywords::extract(&found);
                let _ = crate::util::keywords::grounded_filename(&found);
            }
        }
        Kind::Audio(ext) => {
            let _ = super::audio_meta::extract(&scratch.join(format!("fuzz.{ext}")), Some(bytes));
            let _ = super::container_guard::symphonia_safe(super::container_guard::Source::Bytes(bytes));
        }
        Kind::Obj => {
            let path = scratch.join("fuzz.obj");
            if std::fs::write(&path, bytes).is_ok() {
                problems.extend(check_frame(super::obj_render::render_obj_to_rgb(&path).ok().as_ref()));
            }
        }
    }
    let _ = crate::util::smart_name::readable_stem(&text, &["Fuzz".into()]);
    let _ = crate::face_bbox::NormalizedFaceBBox::parse(&text, 640, 480, bytes.first().copied().unwrap_or(0) % 10);
}

#[test]
fn decoder_fuzzer_never_panics_hangs_or_overruns_the_pixel_cap() {
    let scratch = std::env::temp_dir().join(format!("fileid-decoder-fuzz-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&scratch).unwrap();
    let total = iterations("FILEID_FUZZ_DECODER_ITERATIONS", 400);
    let heavy = std::env::var_os("FILEID_FUZZ_DECODER_ITERATIONS").is_some();
    let seed = iterations("FILEID_FUZZ_SEED", 0x0DEC_0DE5) as u64;
    let (panics, restore_hook) = record_panics("fileid-fuzz-decoders");
    let worker_scratch = scratch.clone();
    let (failures, stats) = std::thread::Builder::new()
        .name("fileid-fuzz-decoders".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let seeds = seeds(heavy);
            let mut rng = Rng::new(seed);
            let mut failures: BTreeMap<String, usize> = BTreeMap::new();
            let mut per_kind: BTreeMap<String, usize> = BTreeMap::new();
            for round in 0..total {
                let (kind, base) = &seeds[round % seeds.len()];
                let bytes = if round < seeds.len() { base.clone() } else { mutate(base.clone(), &mut rng) };
                let started = Instant::now();
                let mut problems = Vec::new();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_one(*kind, &bytes, &worker_scratch, &mut problems)));
                if outcome.is_err() {
                    problems.push("panic escaped the decoder".into());
                }
                if started.elapsed() > Duration::from_secs(10) {
                    problems.push(format!("took {:?}", started.elapsed()));
                }
                for problem in problems {
                    *failures.entry(format!("{kind:?}: {problem}")).or_default() += 1;
                }
                *per_kind.entry(format!("{kind:?}")).or_default() += 1;
            }
            (failures, per_kind)
        })
        .unwrap()
        .join()
        .unwrap();
    restore_hook();
    let _ = std::fs::remove_dir_all(&scratch);
    let mut failures = failures;
    for (signature, count) in panics.lock().iter() {
        failures.insert(format!("panic: {signature}"), *count);
    }
    eprintln!("decoder fuzzer: seed {seed:#x}, {total} inputs, {stats:?}");
    assert!(failures.is_empty(), "decoder fuzzer found {} failure signatures:\n{failures:#?}", failures.len());
}
