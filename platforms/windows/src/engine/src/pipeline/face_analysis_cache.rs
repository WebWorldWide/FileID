use rusqlite::{params, Connection, OptionalExtension};

pub const PROCESSING_VERSION: &str = "rust-landmark-overlap-v2-orientation";
pub const BATCH_SIZE: usize = 5000;

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub id: i64,
    pub file_id: i64,
    pub bbox: String,
    pub path: String,
    pub size: i64,
    pub modified: Option<f64>,
    pub file_ref: Option<i64>,
}

impl Input {
    pub fn revision(&self) -> String {
        format!("{}:{}", self.size, self.modified.map_or_else(|| "unknown".into(), |value| value.to_bits().to_string()))
    }
}

pub struct Page {
    pub cursor: Option<i64>,
    pub inputs: Vec<Input>,
}

pub fn normalized(blob: &[u8]) -> bool {
    if blob.len() != 512 { return false; }
    let mut norm = 0.0;
    for bytes in blob.chunks_exact(4) {
        let value = f32::from_le_bytes(bytes.try_into().expect("four-byte chunk"));
        if !value.is_finite() { return false; }
        norm += f64::from(value).powi(2);
    }
    (0.95..=1.05).contains(&norm)
}

pub fn pending(conn: &Connection, model: &str, after_id: i64, limit: usize, now: f64) -> anyhow::Result<Page> {
    anyhow::ensure!(limit > 0 && limit <= BATCH_SIZE && now.is_finite(), "Invalid face refresh page");
    let mut query = conn.prepare(
        "SELECT fp.id,fp.file_id,fp.bbox,f.path_text,f.size_bytes,f.modified_at,f.file_ref,\
         CASE WHEN length(fp.arcface_embedding)=512 THEN fp.arcface_embedding ELSE X'' END,fp.embedding_model,fp.processing_version,fp.source_revision,r.revision \
         FROM face_prints fp JOIN files f ON f.id=fp.file_id LEFT JOIN catalog_revisions r ON r.file_id=f.id \
         WHERE f.failed=0 AND COALESCE(fp.excluded,0)=0 AND fp.id>?1 AND NOT EXISTS (\
           SELECT 1 FROM face_refresh_failures q WHERE q.face_id=fp.id AND q.embedding_model=?2 \
           AND q.processing_version=?3 AND q.source_path=f.path_text AND q.source_size=f.size_bytes \
           AND q.source_modified IS f.modified_at AND q.bbox=fp.bbox AND q.retry_after>?4) \
         ORDER BY fp.id LIMIT ?5")?;
    let mut rows = query.query(params![after_id, model, PROCESSING_VERSION, now, limit as i64])?;
    let mut page = Page { cursor: None, inputs: Vec::new() };
    while let Some(row) = rows.next()? {
        let input = Input { id: row.get(0)?, file_id: row.get(1)?, bbox: row.get(2)?, path: row.get(3)?,
            size: row.get(4)?, modified: row.get(5)?, file_ref: row.get(6)? };
        page.cursor = Some(input.id);
        let blob: Option<Vec<u8>> = row.get(7)?;
        let stored_model: Option<String> = row.get(8)?;
        let processing: Option<String> = row.get(9)?;
        let source: Option<String> = row.get(10)?;
        let current: Option<String> = row.get(11)?;
        if stored_model.as_deref() == Some(model) && processing.as_deref() == Some(PROCESSING_VERSION)
            && source.as_deref() == Some(input.revision().as_str()) && source == current
            && blob.as_deref().is_some_and(normalized) { continue; }
        page.inputs.push(input);
    }
    Ok(page)
}

fn unchanged(conn: &Connection, input: &Input) -> anyhow::Result<bool> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM face_prints fp JOIN files f ON f.id=fp.file_id \
         WHERE fp.id=?1 AND fp.file_id=?2 AND fp.bbox=?3 AND COALESCE(fp.excluded,0)=0 AND f.failed=0 \
         AND f.path_text=?4 AND f.size_bytes=?5 AND f.modified_at IS ?6 AND f.file_ref IS ?7",
        params![input.id,input.file_id,input.bbox,input.path,input.size,input.modified,input.file_ref],
        |row| row.get::<_, i64>(0))? == 1)
}

pub fn persist(conn: &Connection, input: &Input, blob: &[u8], model: &str, now: f64) -> anyhow::Result<bool> {
    anyhow::ensure!(normalized(blob) && !model.is_empty() && model.len() <= 150 && now.is_finite(), "Invalid refreshed face vector");
    if !unchanged(conn, input)? { return Ok(false); }
    let revision = input.revision();
    conn.execute("INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) \
        VALUES(?1,?2,'file-stat-v1',?3) ON CONFLICT(file_id) DO UPDATE SET revision=excluded.revision,\
        processing_version=excluded.processing_version,updated_at=excluded.updated_at WHERE catalog_revisions.revision!=excluded.revision",
        params![input.file_id,revision,now])?;
    let changed = conn.execute("UPDATE face_prints SET arcface_embedding=?1,embedding_model=?2,processing_version=?3,source_revision=?4 \
        WHERE id=?5 AND file_id=?6 AND bbox=?7 AND COALESCE(excluded,0)=0",
        params![blob,model,PROCESSING_VERSION,revision,input.id,input.file_id,input.bbox])? == 1;
    if changed { conn.execute("DELETE FROM face_refresh_failures WHERE face_id=?1", [input.id])?; }
    Ok(changed)
}

pub fn record_failure(conn: &Connection, input: &Input, model: &str, reason: &str, now: f64) -> anyhow::Result<bool> {
    anyhow::ensure!(!model.is_empty() && model.len() <= 150 && now.is_finite()
        && ["source_unavailable_or_changed","no_embedding"].contains(&reason), "Invalid face refresh failure");
    if !unchanged(conn, input)? { return Ok(false); }
    let attempts = conn.query_row("SELECT attempts FROM face_refresh_failures WHERE face_id=?1 \
        AND embedding_model=?2 AND processing_version=?3 AND source_path=?4 AND source_size=?5 \
        AND source_modified IS ?6 AND bbox=?7",
        params![input.id,model,PROCESSING_VERSION,input.path,input.size,input.modified,input.bbox],
        |row| row.get::<_, i64>(0)).optional()?.unwrap_or(0).saturating_add(1).min(16);
    let delay = (60.0 * 2_f64.powi((attempts - 1) as i32)).min(3600.0);
    conn.execute("INSERT INTO face_refresh_failures(face_id,embedding_model,processing_version,source_revision,\
        source_path,source_size,source_modified,bbox,attempts,retry_after,reason,updated_at) \
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) ON CONFLICT(face_id) DO UPDATE SET \
        embedding_model=excluded.embedding_model,processing_version=excluded.processing_version,source_revision=excluded.source_revision,\
        source_path=excluded.source_path,source_size=excluded.source_size,source_modified=excluded.source_modified,bbox=excluded.bbox,\
        attempts=excluded.attempts,retry_after=excluded.retry_after,reason=excluded.reason,updated_at=excluded.updated_at",
        params![input.id,model,PROCESSING_VERSION,input.revision(),input.path,input.size,input.modified,input.bbox,
            attempts,now+delay,reason,now])?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute_batch("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,'/tmp/face.png',X'01',4,42,1,'image','png'); INSERT INTO persons(id,name,created_at) VALUES(7,'Confirmed',1);").unwrap();
        conn
    }

    fn blob() -> Vec<u8> {
        let mut bytes = vec![0_u8;512];
        bytes[..4].copy_from_slice(&1_f32.to_le_bytes());
        bytes
    }

    fn insert(conn: &Connection, id: i64) {
        conn.execute("INSERT INTO face_prints(id,file_id,person_id,print_data,bbox) VALUES(?1,1,7,X'00','0.1,0.2,0.3,0.4')",[id]).unwrap();
    }

    #[test]
    fn refresh_retry_is_scoped_bounded_and_cleared_by_success() {
        let conn = database(); insert(&conn,1);
        let input = pending(&conn,"model",0,10,100.0).unwrap().inputs.remove(0);
        assert!(record_failure(&conn,&input,"model","no_embedding",100.0).unwrap());
        assert!(pending(&conn,"model",0,10,159.0).unwrap().inputs.is_empty());
        assert_eq!(pending(&conn,"model",0,10,160.0).unwrap().inputs.len(),1);
        assert_eq!(pending(&conn,"other-model",0,10,101.0).unwrap().inputs.len(),1);
        for attempt in 2..=20 {
            assert!(record_failure(&conn,&input,"model","no_embedding",100.0).unwrap());
            let (count,retry): (i64,f64) = conn.query_row("SELECT attempts,retry_after FROM face_refresh_failures",[],|row| Ok((row.get(0)?,row.get(1)?))).unwrap();
            assert_eq!(count,attempt.min(16));
            assert!(retry <= 3700.0);
        }
        assert!(persist(&conn,&input,&blob(),"model",101.0).unwrap());
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM face_refresh_failures",[],|row| row.get::<_,i64>(0)).unwrap(),0);
        assert!(pending(&conn,"model",0,10,102.0).unwrap().inputs.is_empty());
        assert_eq!(conn.query_row("SELECT name FROM persons WHERE id=7",[],|row| row.get::<_,String>(0)).unwrap(),"Confirmed");
        assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=1",[],|row| row.get::<_,i64>(0)).unwrap(),7);
    }

    #[test]
    fn pages_advance_past_healthy_prefix_and_recover_corrupt_current_vectors() {
        let mut conn = database();
        {
            let tx = conn.transaction().unwrap();
            for id in 1..=5001 {
                insert(&tx,id);
                let input = Input{id,file_id:1,bbox:"0.1,0.2,0.3,0.4".into(),path:"/tmp/face.png".into(),size:4,modified:Some(42.0),file_ref:None};
                if id <= 5000 { assert!(persist(&tx,&input,&blob(),"model",100.0).unwrap()); }
            }
            tx.commit().unwrap();
        }
        let first = pending(&conn,"model",0,BATCH_SIZE,101.0).unwrap();
        assert_eq!(first.cursor,Some(5000)); assert!(first.inputs.is_empty());
        assert_eq!(pending(&conn,"model",first.cursor.unwrap(),BATCH_SIZE,101.0).unwrap().inputs[0].id,5001);
        let mut corrupt = blob(); corrupt[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        conn.execute("UPDATE face_prints SET arcface_embedding=?1 WHERE id=3",[corrupt]).unwrap();
        assert_eq!(pending(&conn,"model",0,BATCH_SIZE,101.0).unwrap().inputs[0].id,3);
        conn.execute("UPDATE face_prints SET arcface_embedding=zeroblob(1048576) WHERE id=4",[]).unwrap();
        let pending_ids: Vec<_> = pending(&conn,"model",0,BATCH_SIZE,101.0).unwrap().inputs.into_iter().map(|input| input.id).collect();
        assert_eq!(pending_ids,vec![3,4]);
    }

    #[test]
    fn edits_invalidate_delay_and_prevent_stale_cache_publication() {
        let conn = database(); insert(&conn,1);
        let old = pending(&conn,"model",0,10,100.0).unwrap().inputs.remove(0);
        record_failure(&conn,&old,"model","no_embedding",100.0).unwrap();
        conn.execute("UPDATE face_prints SET bbox='0.2,0.2,0.3,0.4' WHERE id=1",[]).unwrap();
        assert_eq!(pending(&conn,"model",0,10,101.0).unwrap().inputs.len(),1);
        assert!(!persist(&conn,&old,&blob(),"model",101.0).unwrap());
        assert!(!record_failure(&conn,&old,"model","no_embedding",101.0).unwrap());
        let current = pending(&conn,"model",0,10,101.0).unwrap().inputs.remove(0);
        conn.execute("UPDATE files SET file_ref=8 WHERE id=1",[]).unwrap();
        assert!(!persist(&conn,&current,&blob(),"model",101.0).unwrap());
        assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=1",[],|row| row.get::<_,i64>(0)).unwrap(),7);
        assert!(persist(&conn,&current,&[0;512],"model",101.0).is_err());
    }
}
