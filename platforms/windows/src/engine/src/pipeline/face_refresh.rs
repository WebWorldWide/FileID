use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use image::ImageDecoder;
use parking_lot::Mutex;

use super::face_analysis_cache::{self as cache, Input};
use super::face_crop_publication::{self as publication, CropPublication};
use crate::face_bbox::NormalizedFaceBBox;
use crate::models::{face_align, sface::SFace, scrfd::Detection, yunet::YuNet};

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceKey { size: u64, modified: u64, file_ref: u64 }

impl SourceKey {
    fn from_file(file: &File) -> Option<Self> {
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() { return None; }
        let modified = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
        Some(Self { size: metadata.len(), modified: modified.to_bits(), file_ref: crate::platform::opened_file_ref(file)? })
    }

    fn matches(&self, input: &Input) -> bool {
        input.size >= 0 && self.size == input.size as u64
            && input.modified.is_some_and(|value| value.is_finite() && value.to_bits() == self.modified)
            && input.file_ref.is_none_or(|value| value as u64 == self.file_ref)
    }

    fn still_current(&self, path: &str, file: &File) -> bool {
        SourceKey::from_file(file).as_ref() == Some(self)
            && File::open(crate::util::path_safety::to_extended_length(Path::new(path))).ok()
                .and_then(|file| SourceKey::from_file(&file)).as_ref() == Some(self)
    }
}

pub(crate) struct Frame { rgb: Vec<u8>, width: u32, height: u32, orientation: u8 }

fn decode(file: &File) -> Result<Frame> {
    let mut reader = image::ImageReader::new(BufReader::new(file.try_clone()?)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    limits.max_image_width = Some(65536);
    limits.max_image_height = Some(65536);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    anyhow::ensure!(width > 0 && height > 0 && u64::from(width) * u64::from(height) <= 50_000_000,
        "Face source exceeds decoded-image bounds");
    let orientation = decoder.orientation()?;
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let rgb = image.into_rgb8();
    Ok(Frame { width: rgb.width(), height: rgb.height(), rgb: rgb.into_raw(), orientation: orientation.to_exif() })
}

pub(crate) struct EmbeddedFace { pub blob: Vec<u8>, pub crop: Vec<u8> }

pub(crate) trait Backend {
    fn model(&self) -> &str;
    fn extract(&mut self, frame: &Frame, inputs: &[Input], cancel: &AtomicBool) -> Result<Vec<Option<EmbeddedFace>>>;
}

struct OnnxBackend { detector: YuNet, embedder: SFace }

impl OnnxBackend {
    fn load() -> Result<Self> {
        anyhow::ensure!(!crate::coordinator::process_gpu_device_removed(), "GPU unavailable; restart FileID before face refresh");
        anyhow::ensure!(crate::ort_runtime::is_available(), "{}", crate::ort_runtime::missing_runtime_message());
        Ok(Self { detector: YuNet::load(crate::models::yunet::default_weights_path()?)
            .context("Install the registered face models in Settings before refreshing People")?,
            embedder: SFace::load(crate::models::sface::default_weights_path()?)
            .context("Install the registered face models in Settings before refreshing People")? })
    }
}

fn landmark_match(bounds: NormalizedFaceBBox, detections: &[Detection], width: u32, height: u32) -> Option<usize> {
    let mut scores = Vec::new();
    for (index, detection) in detections.iter().enumerate() {
        let [x, y, right, bottom] = detection.bbox.map(f64::from);
        let (x, y, w, h) = (x / f64::from(width), y / f64::from(height),
            (right - x) / f64::from(width), (bottom - y) / f64::from(height));
        if ![x,y,w,h].iter().all(|value| value.is_finite()) || x < 0.0 || y < 0.0 || w <= 0.0 || h <= 0.0 || x+w > 1.0 || y+h > 1.0 { continue; }
        let area = ((bounds.x+bounds.width).min(x+w)-bounds.x.max(x)).max(0.0)
            * ((bounds.y+bounds.height).min(y+h)-bounds.y.max(y)).max(0.0);
        let overlap = area / (bounds.width*bounds.height+w*h-area);
        let dx = (bounds.x+bounds.width/2.0-x-w/2.0).abs()/bounds.width.min(w);
        let dy = (bounds.y+bounds.height/2.0-y-h/2.0).abs()/bounds.height.min(h);
        if overlap >= 0.5 && dx <= 0.35 && dy <= 0.35 { scores.push((index, overlap)); }
    }
    scores.sort_by(|a,b| b.1.total_cmp(&a.1));
    let first = scores.first()?;
    if scores.get(1).is_some_and(|second| first.1-second.1 < 0.1) { return None; }
    Some(first.0)
}

fn crop(frame: &Frame, bbox: &str, detections: &[Detection]) -> Option<Vec<u8>> {
    if bbox.trim().starts_with('{') {
        let json: serde_json::Value = serde_json::from_str(bbox).ok()?;
        if json.get("sourceWidth").is_some() || json.get("sourceHeight").is_some() {
            let (width,height) = if frame.orientation >= 5 { (frame.height,frame.width) } else { (frame.width,frame.height) };
            if json.get("sourceWidth")?.as_f64()?.to_bits() != f64::from(width).to_bits()
                || json.get("sourceHeight")?.as_f64()?.to_bits() != f64::from(height).to_bits() { return None; }
        }
    }
    let bounds = NormalizedFaceBBox::parse(bbox, frame.width, frame.height, frame.orientation)?;
    if bounds.x < 0.0 || bounds.y < 0.0 || bounds.x+bounds.width > 1.0 || bounds.y+bounds.height > 1.0 { return None; }
    if let Some(index) = landmark_match(bounds, detections, frame.width, frame.height) {
        return face_align::align_112(&frame.rgb, frame.width, frame.height, &detections[index].landmarks);
    }
    let image = image::ImageBuffer::<image::Rgb<u8>, &[u8]>::from_raw(frame.width, frame.height, frame.rgb.as_slice())?;
    let x = ((bounds.x-bounds.width*0.15).max(0.0)*f64::from(frame.width)).floor() as u32;
    let y = ((bounds.y-bounds.height*0.15).max(0.0)*f64::from(frame.height)).floor() as u32;
    let right = ((bounds.x+bounds.width*1.15).min(1.0)*f64::from(frame.width)).ceil() as u32;
    let bottom = ((bounds.y+bounds.height*1.15).min(1.0)*f64::from(frame.height)).ceil() as u32;
    if right <= x+4 || bottom <= y+4 { return None; }
    let region = image::imageops::crop_imm(&image,x,y,right-x,bottom-y);
    Some(image::imageops::resize(&*region,112,112,image::imageops::FilterType::Triangle).into_raw())
}

impl Backend for OnnxBackend {
    fn model(&self) -> &str { self.embedder.model_version() }
    fn extract(&mut self, frame: &Frame, inputs: &[Input], cancel: &AtomicBool) -> Result<Vec<Option<EmbeddedFace>>> {
        let detections = self.detector.detect(&frame.rgb,frame.width,frame.height)?;
        let mut faces = Vec::with_capacity(inputs.len());
        for input in inputs {
            if cancel.load(Ordering::Relaxed) { break; }
            let Some(crop) = crop(frame,&input.bbox,&detections) else { faces.push(None); continue; };
            let values = self.embedder.embed(&crop)?;
            faces.push(Some(EmbeddedFace { blob: values.iter().flat_map(|value| value.to_le_bytes()).collect(), crop }));
        }
        Ok(faces)
    }
}

struct StagedCrop(PathBuf);
impl Drop for StagedCrop { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }

fn stage_crop(dir: &Path, input: &Input, pixels: &[u8]) -> Result<StagedCrop> {
    crate::util::read_only::require_writable(dir)?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.{}.pending",input.id,uuid::Uuid::new_v4()));
    crate::util::read_only::require_writable(&path)?;
    let staged = StagedCrop(path);
    let image = image::RgbImage::from_raw(112,112,pixels.to_vec()).context("Invalid refreshed face crop")?;
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image).write_to(&mut bytes,image::ImageFormat::Jpeg)?;
    let mut file = File::options().write(true).create_new(true).open(&staged.0)?;
    file.write_all(bytes.get_ref())?;
    file.sync_all()?;
    Ok(staged)
}

fn now() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64() }

pub(crate) fn run_with_backend(db: &Arc<Mutex<rusqlite::Connection>>, backend: &mut impl Backend,
    cancel: &AtomicBool, crops_dir: &Path) -> Result<usize> {
    publication::recover(&db.lock(),crops_dir)?;
    let model = backend.model().to_owned();
    let mut cursor = 0;
    let mut saved = 0;
    loop {
        if cancel.load(Ordering::Relaxed) { return Ok(saved); }
        let page = cache::pending(&db.lock(),&model,cursor,cache::BATCH_SIZE,now())?;
        let Some(next) = page.cursor else { return Ok(saved); };
        cursor = next;
        let mut by_file = BTreeMap::<i64,Vec<Input>>::new();
        for input in page.inputs { by_file.entry(input.file_id).or_default().push(input); }
        for inputs in by_file.values() {
            if cancel.load(Ordering::Relaxed) { return Ok(saved); }
            let first = &inputs[0];
            let source = File::open(crate::util::path_safety::to_extended_length(Path::new(&first.path))).ok();
            let key = source.as_ref().and_then(SourceKey::from_file);
            let usable = key.as_ref().is_some_and(|key| inputs.iter().all(|input| key.matches(input)));
            let result = if usable { source.as_ref().map(|file| decode(file).and_then(|frame| backend.extract(&frame,inputs,cancel))) }
                else { None };
            if cancel.load(Ordering::Relaxed) { return Ok(saved); }
            if let Some(Err(error)) = &result {
                if crate::models::runtime::error_has_device_removed_marker(error) {
                    crate::coordinator::mark_global_gpu_dead();
                    anyhow::bail!("GPU unavailable; face refresh stopped");
                }
            }
            anyhow::ensure!(!crate::coordinator::process_gpu_device_removed(), "GPU unavailable; face refresh stopped");
            let current = match (&source,&key) { (Some(file),Some(key)) => key.still_current(&first.path,file), _ => false };
            let faces = result.and_then(|result| result.ok()).filter(|faces| faces.len()==inputs.len());
            for (index,input) in inputs.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) { return Ok(saved); }
                let face = faces.as_ref().and_then(|faces| faces[index].as_ref())
                    .filter(|face| cache::normalized(&face.blob) && current);
                let staged = face.map(|face| stage_crop(crops_dir,input,&face.crop)).transpose()?;
                if cancel.load(Ordering::Relaxed) { return Ok(saved); }
                let mut conn = db.lock();
                let tx = conn.transaction()?;
                let mut publication = None;
                if let (Some(face), Some(staged)) = (face, staged) {
                    let live = match (&source,&key) { (Some(file),Some(key)) => key.still_current(&first.path,file), _ => false };
                    let old = publication::evidence(&tx,input.id)?;
                    if live && cache::persist(&tx,input,&face.blob,&model,now())? {
                        let dest = crops_dir.join(format!("{}.jpg",input.id));
                        let new = publication::evidence(&tx,input.id)?.context("Refreshed face disappeared")?;
                        publication = Some(CropPublication::publish(&staged.0,dest,input.id,old,new)?);
                    }
                } else {
                    cache::record_failure(&tx,input,&model,if current { "no_embedding" } else { "source_unavailable_or_changed" },now())?;
                }
                tx.commit()?;
                if let Some(publication) = publication { publication.commit(); saved += 1; }
            }
        }
    }
}

fn refresh_needed(db: &Arc<Mutex<rusqlite::Connection>>, model: &str, cancel: &AtomicBool) -> Result<bool> {
    let mut cursor = 0;
    loop {
        if cancel.load(Ordering::Relaxed) { return Ok(false); }
        let page = cache::pending(&db.lock(),model,cursor,cache::BATCH_SIZE,now())?;
        if !page.inputs.is_empty() { return Ok(true); }
        let Some(next) = page.cursor else { return Ok(false); };
        cursor = next;
    }
}

pub(crate) fn run(db: &Arc<Mutex<rusqlite::Connection>>, cancel: &AtomicBool) -> Result<Option<String>> {
    if cancel.load(Ordering::Relaxed) { return Ok(None); }
    let crops = crate::paths::faces_dir()?;
    publication::recover(&db.lock(),&crops)?;
    let exists: bool = db.lock().query_row("SELECT EXISTS(SELECT 1 FROM face_prints WHERE COALESCE(excluded,0)=0)",[],|row| row.get(0))?;
    if !exists { return Ok(None); }
    let selected = SFace::fingerprint(crate::models::sface::default_weights_path()?)?;
    if !refresh_needed(db,&selected,cancel)? { return Ok(Some(selected)); }
    let mut backend = OnnxBackend::load()?;
    anyhow::ensure!(backend.model() == selected, "Face model selection changed during refresh admission");
    run_with_backend(db,&mut backend,cancel,&crops)?;
    Ok(Some(backend.model().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture { root: PathBuf, db: Arc<Mutex<rusqlite::Connection>>, source: PathBuf }
    impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); } }

    fn fixture() -> Fixture {
        let root = std::env::temp_dir().join(format!("fileid-face-worker-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.png");
        let mut image = image::RgbImage::from_pixel(84,64,image::Rgb([0,0,255]));
        for y in 8..24 { for x in 8..24 { image.put_pixel(x,y,image::Rgb([0,255,0])); } }
        image.save(&source).unwrap();
        let file = File::open(&source).unwrap();
        let key = SourceKey::from_file(&file).unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,file_ref,scanned_at,kind,extension) VALUES(1,?1,X'01',?2,?3,?4,1,'image','png')",
            rusqlite::params![source.to_str().unwrap(),key.size as i64,f64::from_bits(key.modified),key.file_ref as i64]).unwrap();
        conn.execute_batch("INSERT INTO persons(id,name,created_at) VALUES(7,'Confirmed',1);").unwrap();
        Fixture { root,db:Arc::new(Mutex::new(conn)),source }
    }

    fn insert(fixture: &Fixture, id: i64) {
        fixture.db.lock().execute("INSERT INTO face_prints(id,file_id,person_id,print_data,bbox) VALUES(?1,1,7,X'00',?2)",
            rusqlite::params![id,r#"{"x":8,"y":8,"w":16,"h":16,"coordinateSpace":"pixel-top-left","sourceWidth":84,"sourceHeight":64}"#]).unwrap();
    }

    struct FakeBackend { calls: usize, db: Option<Arc<Mutex<rusqlite::Connection>>>, cancel_after: bool, replace: Option<PathBuf> }
    impl FakeBackend { fn new() -> Self { Self { calls:0,db:None,cancel_after:false,replace:None } } }
    impl Backend for FakeBackend {
        fn model(&self) -> &'static str { "fixture-model" }
        fn extract(&mut self, frame: &Frame, inputs: &[Input], cancel: &AtomicBool) -> Result<Vec<Option<EmbeddedFace>>> {
            self.calls += 1;
            let faces = inputs.iter().map(|input| crop(frame,&input.bbox,&[]).map(|crop| {
                let mut blob = vec![0_u8;512]; blob[..4].copy_from_slice(&1_f32.to_le_bytes());
                EmbeddedFace { blob,crop }
            })).collect();
            if let Some(db) = &self.db { db.lock().execute("UPDATE face_prints SET bbox='0,0,0.5,0.5' WHERE id=?1",[inputs[0].id])?; }
            if let Some(path) = &self.replace {
                let old = File::open(path)?;
                let metadata = old.metadata()?;
                let replacement = path.with_extension("replacement");
                std::fs::copy(path,&replacement)?;
                File::options().write(true).open(&replacement)?.set_times(std::fs::FileTimes::new().set_modified(metadata.modified()?))?;
                std::fs::rename(replacement,path)?;
            }
            if self.cancel_after { cancel.store(true,Ordering::Relaxed); }
            Ok(faces)
        }
    }

    #[test]
    fn actual_decode_crop_persistence_and_resume_keep_confirmed_people() {
        let fixture = fixture(); insert(&fixture,1); insert(&fixture,2);
        let source = std::fs::read(&fixture.source).unwrap();
        let mut backend = FakeBackend::new();
        let crops = fixture.root.join("crops");
        let cancel = AtomicBool::new(false);
        assert_eq!(run_with_backend(&fixture.db,&mut backend,&cancel,&crops).unwrap(),2);
        assert_eq!(backend.calls,1);
        let image = image::open(crops.join("1.jpg")).unwrap().into_rgb8();
        let center = image.get_pixel(56,56);
        assert!(center[1]>200 && center[2]<40);
        let conn = fixture.db.lock();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM face_prints WHERE person_id=7 AND processing_version=?1",[cache::PROCESSING_VERSION],|row| row.get::<_,i64>(0)).unwrap(),2);
        assert_eq!(conn.query_row("SELECT name FROM persons WHERE id=7",[],|row| row.get::<_,String>(0)).unwrap(),"Confirmed");
        drop(conn);
        assert_eq!(run_with_backend(&fixture.db,&mut backend,&cancel,&crops).unwrap(),0);
        assert_eq!(backend.calls,1);
        assert_eq!(std::fs::read(&fixture.source).unwrap(),source);
    }

    #[test]
    fn same_stat_source_replacement_is_rejected_and_recorded_without_touching_identity() {
        let fixture = fixture(); insert(&fixture,1);
        let mut backend = FakeBackend::new(); backend.replace = Some(fixture.source.clone());
        assert_eq!(run_with_backend(&fixture.db,&mut backend,&AtomicBool::new(false),&fixture.root.join("crops")).unwrap(),0);
        let conn = fixture.db.lock();
        assert_eq!(conn.query_row("SELECT reason FROM face_refresh_failures WHERE face_id=1",[],|row| row.get::<_,String>(0)).unwrap(),"source_unavailable_or_changed");
        assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=1",[],|row| row.get::<_,i64>(0)).unwrap(),7);
        assert!(!fixture.root.join("crops/1.jpg").exists());
    }

    #[test]
    fn cancellation_and_bbox_edits_do_not_publish_vectors_or_failure_records() {
        for edit in [false,true] {
            let fixture = fixture(); insert(&fixture,1);
            let mut backend = FakeBackend::new();
            backend.cancel_after = !edit;
            if edit { backend.db = Some(fixture.db.clone()); }
            assert_eq!(run_with_backend(&fixture.db,&mut backend,&AtomicBool::new(false),&fixture.root.join("crops")).unwrap(),0);
            let conn = fixture.db.lock();
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM face_prints WHERE arcface_embedding IS NOT NULL",[],|row| row.get::<_,i64>(0)).unwrap(),0);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM face_refresh_failures",[],|row| row.get::<_,i64>(0)).unwrap(),0);
            assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=1",[],|row| row.get::<_,i64>(0)).unwrap(),7);
            assert!(!fixture.root.join("crops/1.jpg").exists());
        }
    }

    #[test]
    fn malformed_or_unavailable_sources_are_durable_retry_candidates() {
        let fixture = fixture(); insert(&fixture,1); insert(&fixture,2);
        fixture.db.lock().execute("UPDATE face_prints SET bbox='bad bounds' WHERE id=2",[]).unwrap();
        let mut backend = FakeBackend::new();
        assert_eq!(run_with_backend(&fixture.db,&mut backend,&AtomicBool::new(false),&fixture.root.join("crops")).unwrap(),1);
        assert_eq!(fixture.db.lock().query_row("SELECT reason FROM face_refresh_failures WHERE face_id=2",[],|row| row.get::<_,String>(0)).unwrap(),"no_embedding");
        assert_eq!(run_with_backend(&fixture.db,&mut backend,&AtomicBool::new(false),&fixture.root.join("crops")).unwrap(),0);
        assert_eq!(backend.calls,1);
        insert(&fixture,3); std::fs::remove_file(&fixture.source).unwrap();
        assert_eq!(run_with_backend(&fixture.db,&mut backend,&AtomicBool::new(false),&fixture.root.join("crops")).unwrap(),0);
        assert_eq!(fixture.db.lock().query_row("SELECT reason FROM face_refresh_failures WHERE face_id=3",[],|row| row.get::<_,String>(0)).unwrap(),"source_unavailable_or_changed");
    }

    #[test]
    fn missing_staged_crop_restores_the_previous_cache() {
        let fixture = fixture(); insert(&fixture,1);
        let destination = fixture.root.join("1.jpg");
        std::fs::write(&destination,b"previous crop").unwrap();
        let staged = StagedCrop(fixture.root.join(format!("1.{}.pending",uuid::Uuid::new_v4())));
        let old = publication::evidence(&fixture.db.lock(),1).unwrap();
        assert!(CropPublication::publish(&staged.0,destination.clone(),1,old.clone(),old.unwrap()).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(),b"previous crop");
        assert!(!std::fs::read_dir(&fixture.root).unwrap().any(|entry| entry.unwrap().path().extension().is_some_and(|extension| extension=="rollback")));
    }

    #[test]
    fn crop_publication_rolls_back_when_database_commit_fails() {
        for existing in [false,true] {
            let fixture = fixture(); insert(&fixture,1);
            let crops = fixture.root.join("crops"); std::fs::create_dir_all(&crops).unwrap();
            let destination = crops.join("1.jpg");
            if existing { std::fs::write(&destination,b"previous crop").unwrap(); }
            fixture.db.lock().execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE commit_guard(face_id INTEGER REFERENCES face_prints(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_face_commit AFTER UPDATE OF arcface_embedding ON face_prints BEGIN INSERT INTO commit_guard VALUES(9999999); END;").unwrap();
            let mut backend = FakeBackend::new();
            assert!(run_with_backend(&fixture.db,&mut backend,&AtomicBool::new(false),&crops).is_err());
            let row: (Option<Vec<u8>>,i64,String) = fixture.db.lock().query_row("SELECT arcface_embedding,person_id,(SELECT name FROM persons WHERE id=person_id) FROM face_prints WHERE id=1",[],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
            assert_eq!(row,(None,7,"Confirmed".into()));
            if existing { assert_eq!(std::fs::read(&destination).unwrap(),b"previous crop"); }
            else { assert!(!destination.exists()); }
            assert_eq!(std::fs::read_dir(&crops).unwrap().count(),usize::from(existing));
        }
    }

    #[test]
    fn healthy_sources_and_delayed_failures_skip_inference_admission() {
        let fixture = fixture(); insert(&fixture,1);
        let cancel = AtomicBool::new(false);
        assert!(refresh_needed(&fixture.db,"fixture-model",&cancel).unwrap());
        let mut backend = FakeBackend::new();
        run_with_backend(&fixture.db,&mut backend,&cancel,&fixture.root.join("crops")).unwrap();
        assert!(!refresh_needed(&fixture.db,"fixture-model",&cancel).unwrap());
        assert!(refresh_needed(&fixture.db,"other-model",&cancel).unwrap());
        insert(&fixture,2);
        let input = cache::pending(&fixture.db.lock(),"fixture-model",0,10,now()).unwrap().inputs.remove(0);
        cache::record_failure(&fixture.db.lock(),&input,"fixture-model","no_embedding",now()).unwrap();
        assert!(!refresh_needed(&fixture.db,"fixture-model",&cancel).unwrap());
        fixture.db.lock().execute("UPDATE face_prints SET bbox='0,0,0.5,0.5' WHERE id=2",[]).unwrap();
        assert!(refresh_needed(&fixture.db,"fixture-model",&cancel).unwrap());
        cancel.store(true,Ordering::Relaxed);
        assert!(!refresh_needed(&fixture.db,"fixture-model",&cancel).unwrap());
    }

    #[test]
    fn refresh_decode_maps_all_jpeg_orientations_and_defers_ambiguous_boxes() {
        let fixture = fixture();
        let raw = image::open(&fixture.source).unwrap().into_rgb8();
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded,100).encode_image(&raw).unwrap();
        let bbox = r#"{"x":8,"y":8,"w":16,"h":16,"coordinateSpace":"pixel-top-left","sourceWidth":84,"sourceHeight":64}"#;
        for orientation in 1..=8 {
            let mut metadata = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
            metadata.extend_from_slice(&[orientation,0,0,0,0,0,0,0]);
            let mut bytes = encoded[..2].to_vec(); bytes.extend_from_slice(&[0xff,0xe1]);
            bytes.extend_from_slice(&((metadata.len()+2) as u16).to_be_bytes());
            bytes.extend_from_slice(&metadata); bytes.extend_from_slice(&encoded[2..]);
            let path = fixture.root.join(format!("{orientation}.jpg")); std::fs::write(&path,&bytes).unwrap();
            let frame = decode(&File::open(&path).unwrap()).unwrap();
            assert_eq!(frame.orientation,orientation);
            assert_eq!((frame.width,frame.height),if orientation>=5 { (64,84) } else { (84,64) });
            let pixels = crop(&frame,bbox,&[]).unwrap();
            let offset = (56*112+56)*3;
            assert!(pixels[offset+1]>200 && pixels[offset+2]<40,"orientation {orientation}");
            assert!(crop(&frame,&bbox.replace("84","83"),&[]).is_none());
            if orientation==6 {
                let native = crop(&frame,"0.625,0.7142857142857143,0.25,0.19047619047619047",&[]).unwrap();
                assert_eq!(pixels,native);
                assert!(crop(&frame,r#"{"x":8,"y":8,"w":16,"h":16}"#,&[]).is_none());
            }
            assert_eq!(std::fs::read(&path).unwrap(),bytes);
        }
    }

    #[test]
    fn landmark_selection_rejects_ambiguous_or_wrong_subjects() {
        let bounds = NormalizedFaceBBox{x:0.1,y:0.2,width:0.3,height:0.4};
        let face = Detection{bbox:[10.0,20.0,40.0,60.0],landmarks:[[0.0;2];5],score:1.0};
        assert_eq!(landmark_match(bounds,std::slice::from_ref(&face),100,100),Some(0));
        assert_eq!(landmark_match(bounds,&[face.clone(),face],100,100),None);
        let other = Detection{bbox:[60.0,20.0,90.0,60.0],landmarks:[[0.0;2];5],score:1.0};
        assert_eq!(landmark_match(bounds,&[other],100,100),None);
    }
}
