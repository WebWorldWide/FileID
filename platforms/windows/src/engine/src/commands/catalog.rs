use crate::ipc::{CatalogChapter, CatalogHit, CatalogJob, CatalogRequest, CatalogResponse, EventPayload, IpcEvent, Wrap};
use crate::ipc::sink::Sink;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Arc;

pub async fn handle(sink: Sink, database: Option<Arc<Mutex<Connection>>>, request: CatalogRequest) {
    let request_id = request.request_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let Some(database) = database else { bail!("The catalog database is unavailable") };
        let mut conn = database.lock();
        execute(&mut conn, &request)
    }).await;
    let response = match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => response(&request_id, "error", Some(error.to_string())),
        Err(error) => response(&request_id, "error", Some(error.to_string())),
    };
    sink.send(IpcEvent { t: chrono::Utc::now(), payload: EventPayload::CatalogResponse(Wrap::new(response)) }).await;
}

fn response(id: &str, status: &str, message: Option<String>) -> CatalogResponse {
    CatalogResponse { request_id: id.into(), status: status.into(), message, hits: vec![], chapters: vec![], jobs: vec![], events: None, takes: None, recommendation: None, suggested_take_groups: None }
}

pub fn execute(conn: &mut Connection, request: &CatalogRequest) -> Result<CatalogResponse> {
    if request.request_id.is_empty() || request.request_id.chars().count() > 200 { bail!("Invalid catalog request") }
    if request.timeline_mode.as_deref().is_some_and(|mode| !["sampled", "moments"].contains(&mode)) {
        bail!("Invalid timeline analysis mode")
    }
    let mut result = response(&request.request_id, "ok", None);
    match request.action.as_str() {
        "search" => {
            if request.search_mode.as_deref().is_some_and(|mode| mode != "keyword")
                || request.query_vector.is_some() || request.embedding_model.is_some()
                || request.result_scope.as_deref().is_some_and(|scope| scope != "all")
            {
                bail!("Catalog vector search is currently available on macOS; PC integration is deferred")
            }
            if request.limit.is_some_and(|limit| !(1..=100).contains(&limit)) {
                bail!("Invalid catalog result limit")
            }
            let query = request.query.as_deref().unwrap_or_default();
            if query.trim().is_empty() || query.chars().count() > 2000 { bail!("Invalid search query") }
            result.hits = search(conn,query,&[])?;
            result.hits.truncate(request.limit.unwrap_or(100));
        }
        "detail" => result.chapters = chapters(conn, request.file_id.ok_or_else(|| anyhow::anyhow!("File selection required"))?)?,
        "saveChapter" => {
            let c = request.chapter.as_ref().ok_or_else(|| anyhow::anyhow!("Chapter required"))?;
            if c.id.is_empty() || c.id.chars().count() > 200 || c.title.trim().is_empty() || c.title.chars().count() > 200 || c.summary.chars().count() > 4000 || !c.start_seconds.is_finite() || !c.end_seconds.is_finite() || c.start_seconds < 0.0 || c.end_seconds < c.start_seconds { bail!("Invalid chapter") }
            let tx = conn.transaction()?;
            let (size, modified): (i64, Option<f64>) = tx.query_row("SELECT size_bytes,modified_at FROM files WHERE id=?1", [c.file_id], |r| Ok((r.get(0)?,r.get(1)?)))?;
            let revision = format!("{}:{}",size,modified.map(|m|m.to_bits().to_string()).unwrap_or_else(||"unknown".into()));
            let owner: Option<i64> = tx.query_row("SELECT file_id FROM catalog_chapters WHERE id=?1", [&c.id], |r|r.get(0)).optional()?;
            if owner.is_some_and(|owner| owner != c.file_id) { bail!("Chapter belongs to another file") }
            let before = chapters(&tx,c.file_id)?.into_iter().find(|chapter| chapter.id == c.id);
            let mut saved = c.clone();
            saved.source_revision.clone_from(&revision);
            saved.model_version = "user".into();
            saved.confidence = 1.0;
            saved.user_edited = true;
            saved.stale = false;
            tx.execute("INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited,stale) VALUES(?1,?2,?3,?4,?5,?6,?7,'user',1,1,0) ON CONFLICT(id) DO UPDATE SET start_seconds=excluded.start_seconds,end_seconds=excluded.end_seconds,title=excluded.title,summary=excluded.summary,source_revision=excluded.source_revision,model_version='user',confidence=1,user_edited=1,stale=0", params![c.id,c.file_id,c.start_seconds,c.end_seconds,c.title,c.summary,revision])?;
            journal_chapter(&tx,c.file_id,&c.id,before.as_ref(),Some(&saved))?;
            tx.commit()?;
            result.chapters = chapters(conn,c.file_id)?;
        }
        "deleteChapter" => {
            let id = request.chapter_id.as_deref().ok_or_else(||anyhow::anyhow!("Chapter required"))?;
            let file = request.file_id.ok_or_else(||anyhow::anyhow!("File selection required"))?;
            let tx = conn.transaction()?;
            let before = chapters(&tx,file)?.into_iter().find(|chapter| chapter.id == id).ok_or_else(||anyhow::anyhow!("Chapter no longer available"))?;
            tx.execute("DELETE FROM catalog_chapters WHERE id=?1 AND file_id=?2", params![id,file])?;
            journal_chapter(&tx,file,id,Some(&before),None)?;
            tx.commit()?;
        }
        "undoChapterEdit" => {
            let file = request.file_id.ok_or_else(||anyhow::anyhow!("File selection required"))?;
            let tx = conn.transaction()?;
            let (id,inverse,after): (String,String,String) = tx.query_row("SELECT o.id,o.inverse_json,c.after_json FROM catalog_operations o JOIN catalog_corrections c ON c.id=o.id WHERE c.file_id=?1 AND c.kind='chapter' AND o.state='completed' ORDER BY o.rowid DESC LIMIT 1", [file], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if let Some(mut before) = serde_json::from_str::<Option<CatalogChapter>>(&inverse)? {
                let (size,modified): (i64,Option<f64>) = tx.query_row("SELECT size_bytes,modified_at FROM files WHERE id=?1", [file], |r|Ok((r.get(0)?,r.get(1)?)))?;
                let revision = format!("{}:{}",size,modified.map(|m|m.to_bits().to_string()).unwrap_or_else(||"unknown".into()));
                before.stale |= before.source_revision != revision;
                tx.execute("INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited,stale) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(id) DO UPDATE SET start_seconds=excluded.start_seconds,end_seconds=excluded.end_seconds,title=excluded.title,summary=excluded.summary,source_revision=excluded.source_revision,model_version=excluded.model_version,confidence=excluded.confidence,user_edited=excluded.user_edited,stale=excluded.stale",params![before.id,file,before.start_seconds,before.end_seconds,before.title,before.summary,before.source_revision,before.model_version,before.confidence,before.user_edited,before.stale])?;
            } else if let Some(after) = serde_json::from_str::<Option<CatalogChapter>>(&after)? {
                tx.execute("DELETE FROM catalog_chapters WHERE id=?1 AND file_id=?2",params![after.id,file])?;
            }
            tx.execute("UPDATE catalog_operations SET state='undone' WHERE id=?1",[id])?;
            tx.commit()?;
            result.chapters=chapters(conn,file)?;
        }
        "jobs" => result.jobs = jobs(conn)?,
        "pauseJob" | "resumeJob" | "cancelJob" => {
            let id = request.job_id.as_deref().ok_or_else(||anyhow::anyhow!("Job required"))?;
            let tx = conn.transaction()?;
            let current: String = tx.query_row("SELECT state FROM catalog_jobs WHERE id=?1", [id], |r|r.get(0))?;
            if !["queued","running","paused"].contains(&current.as_str()) || (request.action=="resumeJob" && current!="paused") { bail!("This job cannot make that transition") }
            if request.action == "resumeJob" { bail!("The timeline worker is not yet available in this engine") }
            let target = match request.action.as_str() {"pauseJob"=>"paused","resumeJob"=>"queued",_=>"cancelled"};
            tx.execute("UPDATE catalog_jobs SET state=?1,updated_at=?2 WHERE id=?3", params![target,chrono::Utc::now().timestamp_millis() as f64/1000.0,id])?;
            tx.commit()?;
            result.jobs = jobs(conn)?;
        }
        "enqueueTimeline" => bail!("Automatic timeline analysis is not yet available in this engine"),
        "listEvents" | "suggestTakeGroups" | "saveEvent" | "deleteEvent" | "undoEventEdit" | "takeGroup" | "setTakeFeedback" | "undoTakeFeedback" => {
            crate::commands::catalog_takes::execute(conn, request, &mut result)?;
        }
        _ => bail!("Unknown catalog action"),
    }
    Ok(result)
}

fn journal_chapter(conn: &Connection, file: i64, chapter_id: &str, before: Option<&CatalogChapter>, after: Option<&CatalogChapter>) -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
    let before_json = serde_json::to_string(&before)?;
    let after_json = serde_json::to_string(&after)?;
    let plan = serde_json::json!({"kind":"chapter","chapterID":chapter_id}).to_string();
    conn.execute("INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?1,?2,'chapter',?3,?4,?5)",params![id,file,before_json,after_json,now])?;
    conn.execute("INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?1,?2,?3,'completed',?4)",params![id,plan,before_json,now])?;
    Ok(())
}

pub(crate) fn chapters(conn: &Connection, file: i64) -> Result<Vec<CatalogChapter>> {
    let mut statement = conn.prepare("SELECT id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited,stale FROM catalog_chapters WHERE file_id=?1 ORDER BY start_seconds,id")?;
    let rows = statement.query_map([file], |r| Ok(CatalogChapter {id:r.get(0)?,file_id:r.get(1)?,start_seconds:r.get(2)?,end_seconds:r.get(3)?,title:r.get(4)?,summary:r.get(5)?,source_revision:r.get(6)?,model_version:r.get(7)?,confidence:r.get(8)?,user_edited:r.get(9)?,stale:r.get(10)?}))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn jobs(conn: &Connection) -> Result<Vec<CatalogJob>> {
    let mut statement = conn.prepare("SELECT id,kind,file_ids_json,state,progress,error,created_at,updated_at FROM catalog_jobs ORDER BY created_at DESC LIMIT 200")?;
    let rows = statement.query_map([], |r| {let json:String=r.get(2)?; let ids=serde_json::from_str(&json).map_err(|e|rusqlite::Error::FromSqlConversionFailure(2,rusqlite::types::Type::Text,Box::new(e)))?;Ok(CatalogJob {id:r.get(0)?,kind:r.get(1)?,file_ids:ids,state:r.get(3)?,progress:r.get(4)?,error:r.get(5)?,created_at:r.get(6)?,updated_at:r.get(7)?})})?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chapter_evidence_is_searchable_and_stale_after_source_change() {
        let mut conn=Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,'/internal/Family Birthday.mov',1,100,10,0,'video','mov')",[]).unwrap();
        let request:CatalogRequest=serde_json::from_value(serde_json::json!({"requestID":"save","action":"saveChapter","chapter":{"id":"gift","fileID":1,"startSeconds":12.5,"endSeconds":20.0,"title":"Gift Opening","summary":"Grandma opens presents","sourceRevision":"untrusted","modelVersion":"untrusted","confidence":0.0,"userEdited":false,"stale":true}})).unwrap();
        let saved=execute(&mut conn,&request).unwrap();
        assert_eq!(saved.chapters[0].model_version,"user");
        assert!(saved.chapters[0].user_edited);
        let query:CatalogRequest=serde_json::from_value(serde_json::json!({"requestID":"search","action":"search","query":"Grandma presents"})).unwrap();
        let hits=execute(&mut conn,&query).unwrap().hits;
        assert_eq!(hits.len(),1);
        assert_eq!(hits[0].start_seconds,Some(12.5));
        conn.execute("INSERT INTO catalog_events(id,title) VALUES('birthday','Birthday')",[]).unwrap();
        conn.execute("INSERT INTO catalog_take_scores(event_id,file_id,outcome_score,quality_score,confidence,explanation,source_revision,model_version,preferred) VALUES('birthday',1,0.9,0.5,0.8,'Fixture','old','fixture',1)",[]).unwrap();
        conn.execute("UPDATE files SET size_bytes=200 WHERE id=1",[]).unwrap();
        let take:(bool,bool)=conn.query_row("SELECT preferred,stale FROM catalog_take_scores WHERE file_id=1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert!(take.0 && take.1);
        assert!(execute(&mut conn,&query).unwrap().hits.is_empty());
        let detail:CatalogRequest=serde_json::from_value(serde_json::json!({"requestID":"detail","action":"detail","fileID":1})).unwrap();
        let detail=execute(&mut conn,&detail).unwrap();
        assert_eq!(detail.chapters[0].title,"Gift Opening");
        assert!(detail.chapters[0].stale);
        let reopened=execute(&mut conn,&request).unwrap();
        assert!(!reopened.chapters[0].stale);
        conn.execute("UPDATE catalog_operations SET created_at=999999999999 WHERE rowid=(SELECT MIN(rowid) FROM catalog_operations)",[]).unwrap();
        let undo:CatalogRequest=serde_json::from_value(serde_json::json!({"requestID":"undo","action":"undoChapterEdit","fileID":1})).unwrap();
        assert!(execute(&mut conn,&undo).unwrap().chapters[0].stale);
        assert!(execute(&mut conn,&undo).unwrap().chapters.is_empty());
        assert!(execute(&mut conn,&undo).is_err());
    }
}

pub(crate) fn search(conn: &Connection, query: &str, kinds: &[String]) -> Result<Vec<CatalogHit>> {
    if !kinds.iter().all(|kind| ["image","video","pdf","doc","audio","other","model"].contains(&kind.as_str())) { bail!("Invalid media filter") }
    let filter = if kinds.is_empty() { None } else { Some(serde_json::to_string(kinds)?) };
            let quoted = query.split_whitespace().filter(|s| s.chars().any(char::is_alphanumeric)).map(|s| format!("\"{}\"", s.replace('"', "\"\""))).collect::<Vec<_>>().join(" ");
            if quoted.is_empty() {
                let Some(filter) = filter else { return Ok(vec![]); };
                let mut statement = conn.prepare("SELECT id,path_text,kind,COALESCE(vlm_description,'') FROM files WHERE kind IN (SELECT value FROM json_each(?1)) ORDER BY id DESC LIMIT 100")?;
                return Ok(statement.query_map([filter], |r| Ok(CatalogHit { file_id:r.get(0)?,path:r.get(1)?,kind:r.get(2)?,text:r.get(3)?,evidence_id:None,start_seconds:None,page:None }))?.collect::<rusqlite::Result<Vec<_>>>()?);
            }
            let mut statement = conn.prepare("SELECT f.id,f.path_text,f.kind,COALESCE(f.vlm_description,'') FROM catalog_file_fts JOIN files f ON f.id=catalog_file_fts.rowid WHERE catalog_file_fts MATCH ?1 AND (?2 IS NULL OR f.kind IN (SELECT value FROM json_each(?2))) ORDER BY bm25(catalog_file_fts) LIMIT 100")?;
            let mut hits = statement.query_map(params![quoted,filter], |r| Ok(CatalogHit { file_id:r.get(0)?,path:r.get(1)?,kind:r.get(2)?,text:r.get(3)?,evidence_id:None,start_seconds:None,page:None }))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut statement = conn.prepare("SELECT f.id,f.path_text,CASE WHEN p.start_seconds IS NOT NULL AND p.confidence=0 THEN 'sampledFrame' ELSE e.kind END,e.text,e.evidence_id,COALESCE(c.start_seconds,p.start_seconds),p.page FROM catalog_evidence_fts e JOIN files f ON f.id=CAST(e.file_id AS INTEGER) LEFT JOIN catalog_chapters c ON c.id=e.evidence_id AND e.kind='chapter' LEFT JOIN catalog_passages p ON p.id=e.evidence_id AND e.kind='passage' WHERE catalog_evidence_fts MATCH ?1 AND (?2 IS NULL OR f.kind IN (SELECT value FROM json_each(?2))) AND (c.stale=0 OR p.stale=0) ORDER BY bm25(catalog_evidence_fts) LIMIT 100")?;
            let evidence = statement.query_map(params![quoted,filter], |r| Ok(CatalogHit { file_id:r.get(0)?,path:r.get(1)?,kind:r.get(2)?,text:r.get(3)?,evidence_id:r.get(4)?,start_seconds:r.get(5)?,page:r.get(6)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
    hits.extend(evidence);
    for (table, kind) in [("doc_fts", "documentText"), ("ocr_fts", "ocrText")] {
        let sql = format!(
            "SELECT f.id,f.path_text,snippet({table},0,'','','…',16) FROM {table} JOIN files f ON f.id={table}.rowid WHERE f.failed=0 AND {table} MATCH ?1 AND (?2 IS NULL OR f.kind IN (SELECT value FROM json_each(?2))) ORDER BY bm25({table}) LIMIT 100"
        );
        let mut statement = conn.prepare(&sql)?;
        let text_hits = statement.query_map(params![quoted, filter], |row| {
            let file_id: i64 = row.get(0)?;
            Ok(CatalogHit {
                file_id,
                path: row.get(1)?,
                kind: kind.to_owned(),
                text: row.get(2)?,
                evidence_id: Some(format!("{kind}:{file_id}")),
                start_seconds: None,
                page: None,
            })
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        hits.extend(text_hits);
    }
    Ok(hits)
}

#[cfg(test)]
mod text_search_tests {
    use super::*;

    #[test]
    fn extracted_document_and_ocr_text_are_catalog_evidence() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,'/internal/notes.txt',1,100,0,'doc','txt')", []).unwrap();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(2,'/internal/photo.jpg',2,100,0,'image','jpg')", []).unwrap();
        conn.execute("INSERT INTO doc_text(file_id,text) VALUES(1,'Birthday gift opening with Alex')", []).unwrap();
        conn.execute("INSERT INTO ocr_text(file_id,text) VALUES(2,'Invoice total 42 dollars')", []).unwrap();

        let documents = search(&conn, "birthday gift", &[]).unwrap();
        assert!(documents.iter().any(|hit| hit.file_id == 1 && hit.kind == "documentText" && hit.text.contains("Birthday gift")));
        assert!(search(&conn, "birthday gift", &["video".to_owned()]).unwrap().is_empty());

        let images = search(&conn, "invoice total", &[]).unwrap();
        assert!(images.iter().any(|hit| hit.file_id == 2 && hit.kind == "ocrText" && hit.text.contains("Invoice total")));

        conn.execute("UPDATE files SET failed=1 WHERE id=1", []).unwrap();
        assert!(search(&conn, "birthday gift", &[]).unwrap().is_empty());
    }
}
