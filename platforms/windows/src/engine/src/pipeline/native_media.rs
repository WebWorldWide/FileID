use super::{validate_export, validate_probe, Operation, Probe, Report};
use anyhow::{bail, Context, Result};
use std::{fs, io::Write, path::Path};
use windows::{
    core::{GUID, HSTRING, PCWSTR, PROPVARIANT},
    Win32::{
        Foundation::{FALSE, TRUE},
        Media::MediaFoundation::{
            IMFMediaType, IMFSourceReader, MFAudioFormat_AAC, MFAudioFormat_PCM,
            MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
            MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_H264, MFVideoFormat_RGB32,
            MFVideoTransFunc_2084, MFVideoTransFunc_HLG, MFSTARTUP_FULL, MF_API_VERSION,
            MF_E_INVALIDSTREAMNUMBER, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
            MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE,
            MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
            MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_ROTATION, MF_PD_DURATION,
            MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
            MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
            MF_SOURCE_READER_FIRST_AUDIO_STREAM, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
            MF_SOURCE_READER_MEDIASOURCE,
        },
        System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED},
    },
};

struct Apartment;
impl Apartment {
    fn enter() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            MFStartup(MF_API_VERSION, MFSTARTUP_FULL)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

pub(super) fn execute(
    source: &Path,
    operation: &Operation,
    output: Option<&Path>,
    report: &mut Report,
) -> Result<()> {
    let _apartment = Apartment::enter()?;
    super::validate_container(source)?;
    if let Operation::Audio {
        start_seconds,
        duration_seconds,
    } = operation
    {
        return unsafe {
            audio(
                source,
                output.context("Missing PCM output")?,
                *start_seconds,
                *duration_seconds,
                report,
            )
        };
    }
    let before = unsafe { probe(source)? };
    validate_probe(&before)?;
    match operation {
        Operation::Probe => {}
        Operation::Frame {
            seconds,
            max_dimension,
        } => unsafe {
            frame(
                source,
                output.context("Missing frame output")?,
                *seconds,
                *max_dimension,
                &before,
                report,
            )?;
        },
        Operation::Video { max_dimension } => {
            if before.hdr {
                bail!("HDR export is unsupported; use an SDR source");
            }
            let path = output.context("Missing video output")?;
            transcode(source, path, *max_dimension, &before)?;
            super::timing::strip_metadata(path)?;
            let after = unsafe { probe(path)? };
            validate_export(&before, &after, *max_dimension).context("This native Windows adapter cannot preserve this input timing, aspect or streams; the staged export was rejected")?;
            report.probe = Some(after);
            return Ok(());
        }
        _ => bail!("Invalid native media operation"),
    }
    report.probe = Some(before);
    Ok(())
}
unsafe fn reader(path: &Path, processing: bool) -> Result<IMFSourceReader> {
    unsafe {
        let path = crate::util::path_safety::strip_extended_length(path);
        let wide: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 2)?;
        let attributes = attributes.context("Missing MF attributes")?;
        attributes.SetUINT32(
            &MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
            u32::from(processing),
        )?;
        MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attributes).map_err(Into::into)
    }
}
unsafe fn dimensions(media: &IMFMediaType) -> Result<(u32, u32)> {
    unsafe {
        let packed = media.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((packed >> 32) as u32, packed as u32);
        if w == 0 || h == 0 || w.max(h) > 8192 || u64::from(w) * u64::from(h) > 32_000_000 {
            bail!("Native frame dimensions exceeded cap");
        }
        Ok((w, h))
    }
}
unsafe fn first_timestamp(reader: &IMFSourceReader, stream: u32) -> Result<f64> {
    unsafe {
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, FALSE)?;
        reader.SetStreamSelection(stream, TRUE)?;
        reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(0i64))?;
        for _ in 0..128 {
            let mut flags = 0;
            let mut timestamp = 0;
            let mut sample = None;
            reader.ReadSample(
                stream,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            if sample.is_some() {
                return Ok(timestamp as f64 / 10_000_000.0);
            }
        }
        bail!("Native stream has no timed samples")
    }
}
unsafe fn range(reader: &IMFSourceReader, index: u32) -> Result<(f64, f64)> {
    unsafe {
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, FALSE)?;
        reader.SetStreamSelection(index, TRUE)?;
        reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(0i64))?;
        let mut start: Option<i64> = None;
        let mut end = 0i64;
        for _ in 0..2_000_000 {
            let mut flags = 0;
            let mut timestamp = 0;
            let mut sample = None;
            reader.ReadSample(
                index,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                let start = start.context("Empty media stream")?;
                if end <= start {
                    bail!("Invalid native stream timestamps");
                }
                return Ok((
                    start as f64 / 10_000_000.0,
                    (end - start) as f64 / 10_000_000.0,
                ));
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                bail!("Changing media types are unsupported");
            }
            if let Some(sample) = sample {
                let duration = sample.GetSampleDuration()?;
                if duration <= 0 || timestamp < 0 {
                    bail!("Invalid native sample timing");
                }
                start = Some(start.map_or(timestamp, |previous| previous.min(timestamp)));
                end = end.max(
                    timestamp
                        .checked_add(duration)
                        .context("Invalid media timestamp")?,
                );
            }
        }
        bail!("Native packet count exceeded cap")
    }
}
unsafe fn probe(path: &Path) -> Result<Probe> {
    unsafe {
        let reader = reader(path, false)?;
        let duration = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)?;
        let duration = u64::try_from(&duration)? as f64 / 10_000_000.0;
        if !duration.is_finite() || duration <= 0.0 || duration > 21600.0 {
            bail!("Unsupported native duration");
        }
        let mut video = None;
        let mut audio = None;
        for index in 0..32 {
            let media = match reader.GetNativeMediaType(index, 0) {
                Ok(media) => media,
                Err(error) if error.code() == MF_E_INVALIDSTREAMNUMBER => break,
                Err(error) => return Err(error.into()),
            };
            let major = media.GetGUID(&MF_MT_MAJOR_TYPE)?;
            if major == MFMediaType_Video && video.is_none() {
                video = Some((index, media));
            } else if major == MFMediaType_Audio && audio.is_none() {
                audio = Some((index, media));
            } else {
                bail!("Only one video and at most one audio track are supported");
            }
            if index == 31 {
                bail!("Native stream count exceeded cap");
            }
        }
        let (video_index, media) = video.context("No video stream")?;
        let (mut width, mut height) = dimensions(&media)?;
        let rotation = media.GetUINT32(&MF_MT_VIDEO_ROTATION).unwrap_or(0);
        if ![0, 90, 180, 270].contains(&rotation) {
            bail!("Unsupported native rotation");
        }
        if rotation == 90 || rotation == 270 {
            std::mem::swap(&mut width, &mut height);
        }
        let aspect = media
            .GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO)
            .unwrap_or((1u64 << 32) | 1);
        if (aspect >> 32) != (aspect & 0xffff_ffff) {
            bail!("Non-square pixel export is unsupported");
        }
        let transfer = media.GetUINT32(&MF_MT_TRANSFER_FUNCTION).unwrap_or(0);
        let hdr = super::source_color(path)?.hdr
            || [
                MFVideoTransFunc_2084.0 as u32,
                MFVideoTransFunc_HLG.0 as u32,
            ]
            .contains(&transfer);
        let subtype = media.GetGUID(&MF_MT_SUBTYPE)?;
        let frame_rate = media.GetUINT64(&MF_MT_FRAME_RATE)?;
        let frame_rate = (frame_rate >> 32) as f64 / (frame_rate as u32) as f64;
        let (video_start, _) = range(&reader, video_index)?;
        let mut probe = Probe {
            duration,
            width,
            height,
            video_codec: if subtype == MFVideoFormat_H264 {
                "h264".into()
            } else {
                format!("{subtype:?}")
            },
            video_start,
            audio_count: u32::from(audio.is_some()),
            audio_codec: None,
            audio_start: None,
            audio_duration: None,
            hdr,
            rotation,
            frame_rate,
        };
        if let Some((index, media)) = audio {
            let subtype = media.GetGUID(&MF_MT_SUBTYPE)?;
            probe.audio_codec = Some(if subtype == MFAudioFormat_AAC {
                "aac".into()
            } else {
                format!("{subtype:?}")
            });
            let (start, duration) = range(&reader, index)?;
            probe.audio_start = Some(start);
            probe.audio_duration = Some(duration);
        }
        let timing = super::timing::inspect(path)?;
        if let Some(video) = timing.video {
            if video.has_edits {
                probe.video_start = video.start;
            }
        }
        if let Some(audio) = timing.audio {
            if audio.has_edits {
                probe.audio_start = Some(audio.start);
            }
            probe.audio_duration = Some(audio.duration);
        }
        Ok(probe)
    }
}
unsafe fn frame(
    source: &Path,
    path: &Path,
    seconds: f64,
    dimension: u32,
    probe: &Probe,
    report: &mut Report,
) -> Result<()> {
    unsafe {
        if seconds >= probe.duration {
            bail!("Requested frame is outside media duration");
        }
        let native_start = first_timestamp(
            &reader(source, false)?,
            MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
        )?;
        let offset = native_start - probe.video_start;
        let reader = reader(source, true)?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, FALSE)?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        reader.SetStreamSelection(stream, TRUE)?;
        let media = MFCreateMediaType()?;
        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        reader.SetCurrentMediaType(stream, None, &media)?;
        reader.SetCurrentPosition(
            &GUID::zeroed(),
            &PROPVARIANT::from(((seconds + offset) * 10_000_000.0) as i64),
        )?;
        for _ in 0..4096 {
            let mut flags = 0;
            let mut timestamp = 0;
            let mut sample = None;
            reader.ReadSample(
                stream,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else {
                continue;
            };
            if timestamp as f64 / 10_000_000.0 - offset + 0.000001 < seconds {
                continue;
            }
            let media = reader.GetCurrentMediaType(stream)?;
            let (width, height) = dimensions(&media)?;
            let stride = media.GetUINT32(&MF_MT_DEFAULT_STRIDE)? as i32;
            if stride <= 0 || stride < (width * 4) as i32 {
                bail!("Unsupported native RGB stride");
            }
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut len = 0;
            buffer.Lock(&mut data, None, Some(&mut len))?;
            let result = (|| -> Result<image::RgbImage> {
                if data.is_null()
                    || u64::from(len) < u64::from(stride as u32) * u64::from(height)
                    || len > 256 * 1024 * 1024
                {
                    bail!("Invalid native RGB buffer");
                }
                let bytes = std::slice::from_raw_parts(data, len as usize);
                Ok(image::RgbImage::from_fn(width, height, |x, y| {
                    let offset = y as usize * stride as usize + x as usize * 4;
                    image::Rgb([bytes[offset + 2], bytes[offset + 1], bytes[offset]])
                }))
            })();
            let _ = buffer.Unlock();
            let image = image::DynamicImage::ImageRgb8(result?);
            let image = match probe.rotation {
                90 => image.rotate90(),
                180 => image.rotate180(),
                270 => image.rotate270(),
                _ => image,
            };
            let image = image.thumbnail(dimension, dimension);
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            super::write_frame_png(&image, file)?;
            report.actual_seconds = Some(timestamp as f64 / 10_000_000.0 - offset);
            report.width = Some(image.width());
            report.height = Some(image.height());
            return Ok(());
        }
        bail!("Native decoder produced no frame")
    }
}
unsafe fn audio(
    source: &Path,
    path: &Path,
    start: f64,
    duration: f64,
    report: &mut Report,
) -> Result<()> {
    unsafe {
        let reader = reader(source, false)?;
        let native_start = first_timestamp(&reader, MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32)?;
        let source_start = if super::validate_container(source)? == "mov" {
            let track = super::timing::inspect(source)?
                .audio
                .context("Missing MP4 audio timing")?;
            if track.has_edits {
                track.start
            } else {
                native_start
            }
        } else {
            native_start
        };
        let offset = source_start - native_start;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, FALSE)?;
        let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
        reader.SetStreamSelection(stream, TRUE)?;
        let media = MFCreateMediaType()?;
        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        media.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1)?;
        media.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 16000)?;
        media.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        media.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2)?;
        media.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 32000)?;
        reader.SetCurrentMediaType(stream, None, &media)?;
        reader.SetCurrentPosition(
            &GUID::zeroed(),
            &PROPVARIANT::from(((start - offset).max(0.0) * 10_000_000.0) as i64),
        )?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let end = start + duration;
        let mut samples = 0u64;
        let mut actual = None;
        for _ in 0..100_000 {
            let mut flags = 0;
            let mut timestamp = 0;
            let mut sample = None;
            reader.ReadSample(
                stream,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else {
                continue;
            };
            let seconds = timestamp as f64 / 10_000_000.0 + offset;
            if seconds >= end {
                break;
            }
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = std::ptr::null_mut();
            let mut len = 0;
            buffer.Lock(&mut data, None, Some(&mut len))?;
            let result = (|| -> Result<()> {
                if data.is_null() || len > 16 * 1024 * 1024 || len % 2 != 0 {
                    bail!("Invalid native PCM buffer");
                }
                let count = len as usize / 2;
                let skip = (((start - seconds).max(0.0) * 16000.0).ceil() as usize).min(count);
                let take = (((end - seconds).max(0.0) * 16000.0).floor() as usize).min(count);
                if take > skip {
                    if samples + (take - skip) as u64 > (duration * 16000.0).ceil() as u64 {
                        bail!("Native PCM sample cap exceeded");
                    }
                    let bytes = std::slice::from_raw_parts(data, len as usize);
                    file.write_all(&bytes[skip * 2..take * 2])?;
                    actual.get_or_insert(seconds + skip as f64 / 16000.0);
                    samples += (take - skip) as u64;
                }
                Ok(())
            })();
            let _ = buffer.Unlock();
            result?;
        }
        if samples == 0 {
            bail!("No PCM in requested window");
        }
        report.actual_seconds = actual;
        report.sample_rate = Some(16000);
        report.samples = Some(samples);
        Ok(())
    }
}
fn transcode(source: &Path, path: &Path, dimension: u32, probe: &Probe) -> Result<()> {
    use windows::{
        Media::{
            MediaProperties::{MediaEncodingProfile, VideoEncodingQuality},
            Transcoding::MediaTranscoder,
        },
        Storage::StorageFile,
    };
    let source = crate::util::path_safety::strip_extended_length(source);
    let path = crate::util::path_safety::strip_extended_length(path);
    let source = StorageFile::GetFileFromPathAsync(&HSTRING::from(
        source.as_os_str().to_string_lossy().as_ref(),
    ))?
    .get()?;
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    let destination = StorageFile::GetFileFromPathAsync(&HSTRING::from(
        path.as_os_str().to_string_lossy().as_ref(),
    ))?
    .get()?;
    let profile = MediaEncodingProfile::CreateMp4(VideoEncodingQuality::HD1080p)?;
    let scale = (f64::from(dimension) / f64::from(probe.width.max(probe.height))).min(1.0);
    let mut width = ((f64::from(probe.width) * scale) as u32 / 2 * 2).max(2);
    let mut height = ((f64::from(probe.height) * scale) as u32 / 2 * 2).max(2);
    if probe.rotation == 90 || probe.rotation == 270 {
        std::mem::swap(&mut width, &mut height);
    }
    profile.Video()?.SetWidth(width)?;
    profile.Video()?.SetHeight(height)?;
    profile
        .Video()?
        .FrameRate()?
        .SetNumerator((probe.frame_rate * 1000.0).round() as u32)?;
    profile.Video()?.FrameRate()?.SetDenominator(1000)?;
    profile.Video()?.PixelAspectRatio()?.SetNumerator(1)?;
    profile.Video()?.PixelAspectRatio()?.SetDenominator(1)?;
    if probe.audio_count == 0 {
        profile.SetAudio(None)?;
    }
    let transcoder = MediaTranscoder::new()?;
    transcoder.SetAlwaysReencode(true)?;
    transcoder.SetHardwareAccelerationEnabled(true)?;
    let prepared = transcoder
        .PrepareFileTranscodeAsync(&source, &destination, &profile)?
        .get()?;
    if !prepared.CanTranscode()? {
        bail!(
            "Native MP4 transcoder cannot encode this input ({:?})",
            prepared.FailureReason()?
        );
    }
    prepared.TranscodeAsync()?.get()?;
    Ok(())
}

pub(super) fn decode_photo(source: &Path) -> Result<image::DynamicImage> {
    use windows::{
        Graphics::Imaging::{
            BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, BitmapTransform,
            ColorManagementMode, ExifOrientationMode,
        },
        Storage::{FileAccessMode, StorageFile},
    };
    let _apartment = Apartment::enter()?;
    let source = crate::util::path_safety::strip_extended_length(source);
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(
        source.as_os_str().to_string_lossy().as_ref(),
    ))?
    .get()?;
    let stream = file.OpenAsync(FileAccessMode::Read)?.get()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.get()?;
    let codec = decoder.DecoderInformation()?.CodecId()?;
    if ![
        BitmapDecoder::PngDecoderId()?,
        BitmapDecoder::JpegDecoderId()?,
        BitmapDecoder::TiffDecoderId()?,
        BitmapDecoder::BmpDecoderId()?,
        BitmapDecoder::HeifDecoderId()?,
    ]
    .contains(&codec)
        || decoder.FrameCount()? != 1
    {
        bail!("Photo requires a single PNG, JPEG, TIFF, BMP or installed native HEIF image");
    }
    if codec == BitmapDecoder::PngDecoderId()? {
        let png = image::codecs::png::PngDecoder::new(std::io::BufReader::new(
            std::fs::File::open(&source)?,
        ))?;
        if png.is_apng()? {
            bail!("Animated photos are unsupported");
        }
    }
    let width = decoder.OrientedPixelWidth()?;
    let height = decoder.OrientedPixelHeight()?;
    if width == 0
        || height == 0
        || width.max(height) > 8192
        || u64::from(width) * u64::from(height) > 32_000_000
    {
        bail!("Photo dimensions exceeded cap");
    }
    let pixels = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Rgba8,
            BitmapAlphaMode::Straight,
            &BitmapTransform::new()?,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::ColorManageToSRgb,
        )?
        .get()?
        .DetachPixelData()?;
    if pixels.len() != width as usize * height as usize * 4 {
        bail!("Native photo decoder returned invalid pixels");
    }
    Ok(image::DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(width, height, pixels.to_vec())
            .context("Invalid native photo buffer")?,
    ))
}
