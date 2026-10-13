use anyhow::{bail, Context, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
#[derive(Clone, Copy)]
pub(super) struct Track {
    pub start: f64,
    pub duration: f64,
    pub has_edits: bool,
}
#[derive(Default)]
pub(super) struct Timing {
    pub video: Option<Track>,
    pub audio: Option<Track>,
}
#[derive(Clone, Copy)]
struct BoxExtent {
    kind: [u8; 4],
    payload: u64,
    end: u64,
    start: u64,
}
fn boxes(file: &mut File, start: u64, end: u64) -> Result<Vec<BoxExtent>> {
    let mut result = Vec::new();
    let mut cursor = start;
    while cursor < end {
        if result.len() >= 100_000 || end - cursor < 8 {
            bail!("Invalid MP4 timing boxes");
        }
        file.seek(SeekFrom::Start(cursor))?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let mut size = u64::from(u32::from_be_bytes(header[..4].try_into()?));
        let mut length = 8;
        if size == 1 {
            let mut wide = [0u8; 8];
            file.read_exact(&mut wide)?;
            size = u64::from_be_bytes(wide);
            length = 16;
        }
        if size == 0 {
            size = end - cursor;
        }
        if size < length || size > end - cursor {
            bail!("Invalid MP4 timing extent");
        }
        result.push(BoxExtent {
            kind: header[4..].try_into()?,
            payload: cursor + length,
            end: cursor + size,
            start: cursor,
        });
        cursor += size;
    }
    Ok(result)
}
fn child(file: &mut File, parent: BoxExtent, kind: &[u8; 4]) -> Result<Option<BoxExtent>> {
    Ok(boxes(file, parent.payload, parent.end)?
        .into_iter()
        .find(|b| &b.kind == kind))
}
fn payload(file: &mut File, extent: BoxExtent, cap: usize) -> Result<Vec<u8>> {
    let size = extent.end - extent.payload;
    if size > cap as u64 {
        bail!("MP4 timing metadata exceeded cap");
    }
    let mut data = vec![0u8; size as usize];
    file.seek(SeekFrom::Start(extent.payload))?;
    file.read_exact(&mut data)?;
    Ok(data)
}
fn times(data: &[u8]) -> Result<(u32, u64)> {
    let (scale, duration) = match data.first() {
        Some(0) if data.len() >= 20 => (
            u32::from_be_bytes(data[12..16].try_into()?),
            u64::from(u32::from_be_bytes(data[16..20].try_into()?)),
        ),
        Some(1) if data.len() >= 32 => (
            u32::from_be_bytes(data[20..24].try_into()?),
            u64::from_be_bytes(data[24..32].try_into()?),
        ),
        _ => bail!("Unsupported MP4 timing header"),
    };
    if scale == 0 {
        bail!("Invalid MP4 timescale");
    }
    Ok((scale, duration))
}
pub(super) fn inspect(path: &Path) -> Result<Timing> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut timing = Timing::default();
    let Some(moov) = boxes(&mut file, 0, length)?
        .into_iter()
        .find(|b| &b.kind == b"moov")
    else {
        return Ok(timing);
    };
    let mvhd = child(&mut file, moov, b"mvhd")?.context("Missing MP4 movie timing")?;
    let (movie_scale, _) = times(&payload(&mut file, mvhd, 128)?)?;
    for track in boxes(&mut file, moov.payload, moov.end)?
        .into_iter()
        .filter(|b| &b.kind == b"trak")
    {
        let mdia = child(&mut file, track, b"mdia")?.context("Missing MP4 track timing")?;
        let hdlr = child(&mut file, mdia, b"hdlr")?.context("Missing MP4 track handler")?;
        let handler = payload(&mut file, hdlr, 65536)?;
        let handler = handler.get(8..12).context("Invalid MP4 handler")?;
        if handler != b"vide" && handler != b"soun" {
            continue;
        }
        let mdhd = child(&mut file, mdia, b"mdhd")?.context("Missing MP4 media timing")?;
        let (media_scale, media_duration) = times(&payload(&mut file, mdhd, 64)?)?;
        let mut range = Track {
            start: 0.0,
            has_edits: false,
            duration: media_duration as f64 / f64::from(media_scale),
        };
        if let Some(edts) = child(&mut file, track, b"edts")? {
            range.has_edits = true;
            let elst = child(&mut file, edts, b"elst")?.context("Missing MP4 edit list")?;
            let edits = payload(&mut file, elst, 512)?;
            if edits.len() < 8 || edits[0] > 1 {
                bail!("Invalid MP4 edit list");
            }
            let count = u32::from_be_bytes(edits[4..8].try_into()?);
            if !(1..=2).contains(&count) {
                bail!("Repeated MP4 edits are unsupported");
            }
            let stride = if edits[0] == 0 { 12 } else { 20 };
            if edits.len() != 8 + count as usize * stride {
                bail!("Invalid MP4 edit extent");
            }
            let mut active = false;
            for index in 0..count as usize {
                let entry = &edits[8 + index * stride..8 + (index + 1) * stride];
                let (duration, media_time, rate_offset) = if stride == 12 {
                    (
                        u64::from(u32::from_be_bytes(entry[..4].try_into()?)),
                        i64::from(i32::from_be_bytes(entry[4..8].try_into()?)),
                        8,
                    )
                } else {
                    (
                        u64::from_be_bytes(entry[..8].try_into()?),
                        i64::from_be_bytes(entry[8..16].try_into()?),
                        16,
                    )
                };
                if entry[rate_offset..] != [0, 1, 0, 0] {
                    bail!("Non-unit MP4 edit rates are unsupported");
                }
                if media_time == -1 && !active {
                    range.start += duration as f64 / f64::from(movie_scale);
                } else if media_time >= 0 && !active {
                    active = true;
                    range.duration = duration as f64 / f64::from(movie_scale);
                    if handler == b"soun" {
                        range.duration = range.duration.min(
                            media_duration.saturating_sub(media_time as u64) as f64
                                / f64::from(media_scale),
                        );
                    }
                } else {
                    bail!("Discontinuous MP4 edits are unsupported");
                }
            }
            if !active {
                bail!("Empty MP4 edit timeline");
            }
        }
        if !range.start.is_finite()
            || !range.duration.is_finite()
            || range.duration <= 0.0
            || range.start + range.duration > 21600.25
        {
            bail!("Invalid bounded MP4 track timing");
        }
        if handler == b"vide" {
            if timing.video.replace(range).is_some() {
                bail!("Multiple MP4 video tracks");
            }
        } else if timing.audio.replace(range).is_some() {
            bail!("Multiple MP4 audio tracks");
        }
    }
    Ok(timing)
}
/// Only the private staged output is rewritten. Box lengths/offsets stay unchanged.
pub(super) fn strip_metadata(path: &Path) -> Result<()> {
    fn strip(file: &mut File, start: u64, end: u64, depth: usize) -> Result<()> {
        if depth > 8 {
            bail!("MP4 metadata nesting exceeded cap");
        }
        for extent in boxes(file, start, end)? {
            match &extent.kind {
                b"udta" | b"meta" | b"uuid" => {
                    file.seek(SeekFrom::Start(extent.start + 4))?;
                    file.write_all(b"free")?;
                    file.seek(SeekFrom::Start(extent.payload))?;
                    let zeros = [0u8; 4096];
                    let mut remaining = extent.end - extent.payload;
                    while remaining > 0 {
                        let count = remaining.min(zeros.len() as u64) as usize;
                        file.write_all(&zeros[..count])?;
                        remaining -= count as u64;
                    }
                }
                b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                    strip(file, extent.payload, extent.end, depth + 1)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let length = file.metadata()?.len();
    strip(&mut file, 0, length, 0)?;
    file.sync_all()?;
    Ok(())
}
