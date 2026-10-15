use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Evidence {
    file_id: i64,
    bbox: String,
    embedding_len: i64,
    embedding: String,
    model: Option<String>,
    processing: Option<String>,
    revision: Option<String>,
}

fn digest(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

pub(super) fn evidence(conn: &Connection, id: i64) -> Result<Option<Evidence>> {
    Ok(conn.query_row("SELECT file_id,bbox,length(arcface_embedding),\
        CASE WHEN length(arcface_embedding)=512 THEN arcface_embedding ELSE X'' END,\
        embedding_model,processing_version,source_revision FROM face_prints WHERE id=?1", [id], |row| {
        Ok(Evidence {
            file_id: row.get(0)?, bbox: digest(row.get::<_,String>(1)?.as_bytes()),
            embedding_len: row.get::<_,Option<i64>>(2)?.unwrap_or(-1),
            embedding: digest(&row.get::<_,Vec<u8>>(3)?),
            model: row.get::<_,Option<String>>(4)?.map(|value| digest(value.as_bytes())),
            processing: row.get::<_,Option<String>>(5)?.map(|value| digest(value.as_bytes())),
            revision: row.get::<_,Option<String>>(6)?.map(|value| digest(value.as_bytes())),
        })
    }).optional()?)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    face_id: i64,
    token: uuid::Uuid,
    old: Option<Evidence>,
    new: Evidence,
    old_crop: Option<String>,
    new_crop: String,
}

impl Journal {
    fn destination(&self, dir: &Path) -> PathBuf { dir.join(format!("{}.jpg",self.face_id)) }
    fn staged(&self, dir: &Path) -> PathBuf { dir.join(format!("{}.{}.pending",self.face_id,self.token)) }
    fn backup(&self, dir: &Path) -> PathBuf { dir.join(format!("{}.{}.rollback",self.face_id,self.token)) }
    fn path(&self, dir: &Path) -> PathBuf { dir.join(format!("{}.{}.publication.json",self.face_id,self.token)) }
    fn validate(&self, path: &Path) -> Result<()> {
        let hash = |value: &str| value.len()==64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        let valid_evidence = |value: &Evidence| value.file_id > 0 && value.embedding_len >= -1
            && hash(&value.bbox) && hash(&value.embedding)
            && [&value.model,&value.processing,&value.revision].iter().all(|field| field.as_ref().is_none_or(|value| hash(value)));
        anyhow::ensure!(self.version==1 && self.face_id>0 && valid_evidence(&self.new)
            && self.old.as_ref().is_none_or(valid_evidence) && hash(&self.new_crop)
            && self.old_crop.as_ref().is_none_or(|value| hash(value))
            && self.path(path.parent().context("Missing crop journal directory")?)==path,
            "Invalid face crop publication journal");
        Ok(())
    }
}

fn regular_file(path: &Path) -> Result<Option<File>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(metadata.is_file() && !metadata.file_type().is_symlink(), "Unsafe face crop cache artifact");
            Ok(Some(File::open(path)?))
        }
        Err(error) if error.kind()==std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn crop_hash(path: &Path) -> Result<Option<String>> {
    let Some(mut file) = regular_file(path)? else { return Ok(None); };
    anyhow::ensure!(file.metadata()?.len()<=4*1024*1024, "Oversized face crop cache artifact");
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(4*1024*1024+1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len()<=4*1024*1024, "Growing face crop cache artifact");
    Ok(Some(digest(&bytes)))
}

fn sync_directory(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn remove_if_hash(path: &Path, expected: &str) -> Result<()> {
    if let Some(actual) = crop_hash(path)? {
        anyhow::ensure!(actual==expected, "Face crop cache artifact changed during recovery");
        crate::util::read_only::require_writable(path)?;
        fs::remove_file(path)?;
    }
    Ok(())
}

fn clean(journal: &Journal, dir: &Path) -> Result<()> {
    if let Some(old) = &journal.old_crop { remove_if_hash(&journal.backup(dir),old)?; }
    else { anyhow::ensure!(crop_hash(&journal.backup(dir))?.is_none(), "Unexpected face crop backup"); }
    remove_if_hash(&journal.staged(dir),&journal.new_crop)?;
    let path = journal.path(dir);
    crate::util::read_only::require_writable(&path)?;
    fs::remove_file(path)?;
    sync_directory(dir)
}

fn restore_old(journal: &Journal, dir: &Path) -> Result<()> {
    let destination = journal.destination(dir);
    let actual = crop_hash(&destination)?;
    if let Some(old) = &journal.old_crop {
        if actual.as_ref()!=Some(old) {
            anyhow::ensure!(crop_hash(&journal.backup(dir))?.as_ref()==Some(old), "Previous face crop unavailable for recovery");
            if actual.is_some() { remove_if_hash(&destination,&journal.new_crop)?; }
            crate::util::read_only::require_writable(&destination)?;
            fs::rename(journal.backup(dir),&destination)?;
        }
    } else if actual.is_some() { remove_if_hash(&destination,&journal.new_crop)?; }
    sync_directory(dir)?;
    clean(journal,dir)
}

fn complete_new(journal: &Journal, dir: &Path) -> Result<()> {
    let destination = journal.destination(dir);
    let actual = crop_hash(&destination)?;
    if actual.as_ref()!=Some(&journal.new_crop) {
        anyhow::ensure!(crop_hash(&journal.staged(dir))?.as_ref()==Some(&journal.new_crop), "Committed face crop unavailable for recovery");
        if let Some(actual) = actual {
            anyhow::ensure!(journal.old_crop.as_ref()==Some(&actual), "Face crop replaced before recovery");
            let backup = journal.backup(dir);
            if crop_hash(&backup)?.is_some() { remove_if_hash(&destination,&actual)?; }
            else {
                crate::util::read_only::require_writable(&backup)?;
                fs::rename(&destination,&backup)?;
            }
        }
        crate::util::read_only::require_writable(&destination)?;
        fs::rename(journal.staged(dir),&destination)?;
    }
    sync_directory(dir)?;
    clean(journal,dir)
}

pub(super) struct CropPublication { dir: PathBuf, journal: Journal, committed: bool }

impl CropPublication {
    fn prepare(staged: &Path, destination: PathBuf, face_id: i64,
        old: Option<Evidence>, new: Evidence) -> Result<Self> {
        crate::util::read_only::require_writable(&destination)?;
        let dir = destination.parent().context("Missing face crop directory")?.to_owned();
        anyhow::ensure!(!fs::symlink_metadata(&dir)?.file_type().is_symlink(), "Unsafe face crop cache directory");
        let name = staged.file_name().and_then(|value| value.to_str()).context("Invalid staged face crop")?;
        let token = name.strip_prefix(&format!("{face_id}.")).and_then(|value| value.strip_suffix(".pending"))
            .context("Invalid staged face crop identity")?.parse::<uuid::Uuid>()?;
        let journal = Journal { version:1,face_id,token,old,new,
            old_crop:crop_hash(&destination)?,new_crop:crop_hash(staged)?.context("Missing staged face crop")? };
        anyhow::ensure!(journal.destination(&dir)==destination && journal.staged(&dir)==staged, "Invalid face crop publication paths");
        let path = journal.path(&dir);
        journal.validate(&path)?;
        crate::util::read_only::require_writable(&path)?;
        let bytes = serde_json::to_vec(&journal)?;
        let draft = path.with_extension("draft");
        crate::util::read_only::require_writable(&draft)?;
        let mut file = File::options().write(true).create_new(true).open(&draft)?;
        let written = file.write_all(&bytes).and_then(|_| file.sync_all());
        drop(file);
        if let Err(error) = written { let _ = fs::remove_file(&draft); return Err(error.into()); }
        if let Err(error) = fs::rename(&draft,&path) { let _ = fs::remove_file(&draft); return Err(error.into()); }
        let publication = Self { dir,journal,committed:false };
        sync_directory(&publication.dir)?;
        Ok(publication)
    }

    fn move_backup(&self) -> Result<()> {
        if self.journal.old_crop.is_some() {
            let backup = self.journal.backup(&self.dir);
            crate::util::read_only::require_writable(&backup)?;
            anyhow::ensure!(regular_file(&backup)?.is_none(), "Face crop backup already exists");
            fs::rename(self.journal.destination(&self.dir),&backup)?;
            sync_directory(&self.dir)?;
        }
        Ok(())
    }

    fn publish_staged(&self) -> Result<()> {
        fs::rename(self.journal.staged(&self.dir),self.journal.destination(&self.dir))?;
        sync_directory(&self.dir)
    }

    pub(super) fn publish(staged: &Path, destination: PathBuf, face_id: i64,
        old: Option<Evidence>, new: Evidence) -> Result<Self> {
        let publication = Self::prepare(staged,destination,face_id,old,new)?;
        publication.move_backup()?;
        publication.publish_staged()?;
        Ok(publication)
    }

    pub(super) fn commit(mut self) { self.committed=true; }
}

impl Drop for CropPublication {
    fn drop(&mut self) {
        let result = if self.committed { complete_new(&self.journal,&self.dir) }
            else { restore_old(&self.journal,&self.dir) };
        if result.is_err() { tracing::warn!("Face crop publication requires journal recovery"); }
    }
}

pub(crate) fn recover(conn: &Connection, dir: &Path) -> Result<usize> {
    if !dir.exists() { return Ok(0); }
    anyhow::ensure!(!fs::symlink_metadata(dir)?.file_type().is_symlink(), "Unsafe face crop cache directory");
    let mut recovered = 0;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.file_name().and_then(|value| value.to_str()).is_some_and(|name| name.ends_with(".publication.json")) { continue; }
        anyhow::ensure!(recovered<10000, "Too many interrupted face crop publications");
        let mut file = regular_file(&path)?.context("Face crop journal disappeared")?;
        anyhow::ensure!(file.metadata()?.len()<=16384, "Oversized face crop journal");
        let mut bytes = Vec::new();
        Read::by_ref(&mut file).take(16385).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len()<=16384, "Growing face crop journal");
        let journal: Journal = serde_json::from_slice(&bytes)?;
        journal.validate(&path)?;
        let current = evidence(conn,journal.face_id)?;
        if current.as_ref()==Some(&journal.new) { complete_new(&journal,dir)?; }
        else if current==journal.old { restore_old(&journal,dir)?; }
        else { anyhow::bail!("Face provenance changed during interrupted crop publication"); }
        recovered+=1;
    }
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    fn vector(index: usize) -> Vec<u8> {
        (0..128).flat_map(|value| if value==index { 1.0f32 } else { 0.0f32 }.to_le_bytes()).collect()
    }

    fn fixture(existing: bool) -> Fixture {
        let root = std::env::temp_dir().join(format!("fileid-crop-publication-{}",uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("crops")).unwrap();
        fs::write(root.join("original.bin"),b"original media remains unchanged").unwrap();
        let conn = Connection::open(root.join("catalog.sqlite")).unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute_batch("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension)\
            VALUES(1,'internal-source',X'01',32,1,1,'image','jpg');\
            INSERT INTO persons(id,name,created_at) VALUES(7,'Confirmed',1);").unwrap();
        conn.execute("INSERT INTO face_prints(id,file_id,bbox,print_data,arcface_embedding,embedding_model,processing_version,source_revision,person_id)\
            VALUES(1,1,'0.1,0.1,0.2,0.2',X'01',?1,'old-model','old-processing','old-revision',7)",[vector(0)]).unwrap();
        if existing { fs::write(root.join("crops/1.jpg"),b"previous crop").unwrap(); }
        Fixture(root)
    }

    fn crash(root: &Path, point: &str) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","pipeline::face_crop_publication::tests::crash_worker","--nocapture"])
            .env("FILEID_CROP_CRASH_TEST_ROOT",root)
            .env("FILEID_CROP_CRASH_TEST_POINT",point)
            .output().unwrap();
        assert_eq!(output.status.code(),Some(73),"{}",String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn crash_worker() {
        let Some(root) = std::env::var_os("FILEID_CROP_CRASH_TEST_ROOT") else { return; };
        let root = PathBuf::from(root);
        assert!(root.starts_with(std::env::temp_dir()));
        assert!(root.file_name().unwrap().to_str().unwrap().starts_with("fileid-crop-publication-"));
        let point = std::env::var("FILEID_CROP_CRASH_TEST_POINT").unwrap();
        let dir = root.join("crops");
        let staged = dir.join(format!("1.{}.pending",uuid::Uuid::new_v4()));
        let mut file = File::options().write(true).create_new(true).open(&staged).unwrap();
        file.write_all(b"refreshed crop").unwrap(); file.sync_all().unwrap(); drop(file);
        let mut conn = Connection::open(root.join("catalog.sqlite")).unwrap();
        let tx = conn.transaction().unwrap();
        let old = evidence(&tx,1).unwrap();
        tx.execute("UPDATE face_prints SET arcface_embedding=?1,embedding_model='new-model',\
            processing_version='new-processing',source_revision='new-revision' WHERE id=1",[vector(1)]).unwrap();
        let new = evidence(&tx,1).unwrap().unwrap();
        let publication = CropPublication::prepare(&staged,dir.join("1.jpg"),1,old,new).unwrap();
        if point=="prepared" { std::process::exit(73); }
        publication.move_backup().unwrap();
        if point=="backup" { std::process::exit(73); }
        publication.publish_staged().unwrap();
        if point=="published" { std::process::exit(73); }
        assert_eq!(point,"committed");
        tx.commit().unwrap();
        std::process::exit(73);
    }

    #[test]
    fn process_exit_reconciles_every_publication_boundary_without_changing_identity_or_source() {
        for existing in [false,true] {
            for point in ["prepared","backup","published","committed"] {
                let fixture = fixture(existing);
                crash(&fixture.0,point);
                let conn = Connection::open(fixture.0.join("catalog.sqlite")).unwrap();
                let before = evidence(&conn,1).unwrap();
                assert_eq!(conn.query_row("SELECT source_revision FROM face_prints WHERE id=1",[],|row| row.get::<_,String>(0)).unwrap(),
                    if point=="committed" { "new-revision" } else { "old-revision" });
                let dir = fixture.0.join("crops");
                assert_eq!(recover(&conn,&dir).unwrap(),1,"{existing}/{point}");
                assert_eq!(evidence(&conn,1).unwrap(),before);
                assert_eq!(recover(&conn,&dir).unwrap(),0);
                assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=1",[],|row| row.get::<_,i64>(0)).unwrap(),7);
                let destination = dir.join("1.jpg");
                if point=="committed" { assert_eq!(fs::read(destination).unwrap(),b"refreshed crop"); }
                else if existing { assert_eq!(fs::read(destination).unwrap(),b"previous crop"); }
                else { assert!(!destination.exists()); }
                assert_eq!(fs::read(fixture.0.join("original.bin")).unwrap(),b"original media remains unchanged");
                assert!(!fs::read_dir(dir).unwrap().any(|entry| entry.unwrap().path().extension().is_some_and(|ext| ext=="pending" || ext=="rollback" || ext=="json")));
            }
        }
    }

    #[test]
    fn unrelated_provenance_and_replaced_cache_are_preserved_for_review() {
        for changed_database in [false,true] {
            let fixture = fixture(true); crash(&fixture.0,"published");
            let conn = Connection::open(fixture.0.join("catalog.sqlite")).unwrap();
            if changed_database { conn.execute("UPDATE face_prints SET bbox='changed bounds' WHERE id=1",[]).unwrap(); }
            else { fs::write(fixture.0.join("crops/1.jpg"),b"independent crop").unwrap(); }
            let db_before = evidence(&conn,1).unwrap();
            let crop_before = fs::read(fixture.0.join("crops/1.jpg")).unwrap();
            assert!(recover(&conn,&fixture.0.join("crops")).is_err());
            assert_eq!(evidence(&conn,1).unwrap(),db_before);
            assert_eq!(fs::read(fixture.0.join("crops/1.jpg")).unwrap(),crop_before);
            assert!(fs::read_dir(fixture.0.join("crops")).unwrap().any(|entry| entry.unwrap().path().extension().is_some_and(|ext| ext=="rollback")));
        }
    }

    #[test]
    fn malformed_journals_and_legacy_backups_are_never_guessed() {
        let fixture = fixture(true);
        let conn = Connection::open(fixture.0.join("catalog.sqlite")).unwrap();
        let dir = fixture.0.join("crops");
        let legacy = dir.join(format!("1.{}.rollback",uuid::Uuid::new_v4()));
        fs::write(&legacy,b"legacy backup").unwrap();
        fs::write(dir.join("1.invalid.publication.draft"),b"incomplete write").unwrap();
        assert_eq!(recover(&conn,&dir).unwrap(),0);
        let malformed = dir.join("1.invalid.publication.json");
        fs::write(&malformed,b"{}").unwrap();
        assert!(recover(&conn,&dir).is_err());
        assert_eq!(fs::read(legacy).unwrap(),b"legacy backup");
        assert_eq!(fs::read(dir.join("1.jpg")).unwrap(),b"previous crop");
        assert_eq!(fs::read(malformed).unwrap(),b"{}");
    }

    #[test]
    fn journal_filename_identity_and_size_are_checked_before_cache_mutation() {
        for oversized in [false,true] {
            let fixture = fixture(true); crash(&fixture.0,"published");
            let conn = Connection::open(fixture.0.join("catalog.sqlite")).unwrap();
            let dir = fixture.0.join("crops");
            let path = fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path())
                .find(|path| path.extension().is_some_and(|ext| ext=="json")).unwrap();
            if oversized { fs::write(&path,vec![b' ';16385]).unwrap(); }
            else { fs::rename(&path,dir.join(format!("2.{}.publication.json",uuid::Uuid::new_v4()))).unwrap(); }
            assert!(recover(&conn,&dir).is_err());
            assert_eq!(fs::read(dir.join("1.jpg")).unwrap(),b"refreshed crop");
            assert!(fs::read_dir(&dir).unwrap().any(|entry| entry.unwrap().path().extension().is_some_and(|ext| ext=="rollback")));
        }
    }

    #[test]
    fn engine_startup_reconciles_before_model_admission() {
        let Some(engine) = std::env::var_os("FILEID_TEST_PORTABLE_ENGINE_PATH") else { return; };
        for point in ["published","committed"] {
            let fixture = fixture(true); crash(&fixture.0,point);
            let data = fixture.0.join("data");
            let state = data.join("FileID");
            fs::create_dir_all(&state).unwrap();
            fs::rename(fixture.0.join("catalog.sqlite"),state.join("fileid.sqlite")).unwrap();
            for suffix in ["-wal","-shm","-journal"] {
                let from = fixture.0.join(format!("catalog.sqlite{suffix}"));
                if from.exists() { fs::rename(from,state.join(format!("fileid.sqlite{suffix}"))).unwrap(); }
            }
            fs::rename(fixture.0.join("crops"),state.join("face_crops")).unwrap();
            let output = std::process::Command::new(&engine)
                .env("XDG_DATA_HOME",&data).env("LOCALAPPDATA",&data)
                .env("FILEID_MODELS_DIR",state.join("Models"))
                .stdin(std::process::Stdio::null()).output().unwrap();
            assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
            assert!(String::from_utf8_lossy(&output.stdout).contains("\"ready\""));
            let conn = Connection::open(state.join("fileid.sqlite")).unwrap();
            assert_eq!(fs::read(state.join("face_crops/1.jpg")).unwrap(),
                if point=="committed" { b"refreshed crop".as_slice() } else { b"previous crop".as_slice() });
            assert_eq!(recover(&conn,&state.join("face_crops")).unwrap(),0);
            assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=1",[],|row| row.get::<_,i64>(0)).unwrap(),7);
            assert_eq!(fs::read(fixture.0.join("original.bin")).unwrap(),b"original media remains unchanged");
        }
    }

    #[cfg(unix)]
    #[test]
    fn journal_symlinks_cannot_escape_the_cache() {
        let fixture = fixture(true);
        let conn = Connection::open(fixture.0.join("catalog.sqlite")).unwrap();
        let original = fixture.0.join("original.bin");
        std::os::unix::fs::symlink(&original,fixture.0.join("crops/1.invalid.publication.json")).unwrap();
        assert!(recover(&conn,&fixture.0.join("crops")).is_err());
        assert_eq!(fs::read(original).unwrap(),b"original media remains unchanged");
    }
}
