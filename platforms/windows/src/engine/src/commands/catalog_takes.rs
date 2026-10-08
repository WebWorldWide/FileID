use crate::ipc::{CatalogEvent, CatalogRequest, CatalogResponse, CatalogTake, CatalogTakeRecommendation};
use anyhow::{bail, ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Score {
    event_id: String,
    file_id: i64,
    outcome_score: Option<f64>,
    quality_score: Option<f64>,
    confidence: f64,
    explanation: String,
    source_revision: String,
    model_version: String,
    preferred: bool,
    stale: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Snapshot {
    event: CatalogEvent,
    scores: Vec<Score>,
}

pub fn execute(conn: &mut Connection, request: &CatalogRequest, result: &mut CatalogResponse) -> Result<()> {
    match request.action.as_str() {
        "listEvents" => result.events = Some(list(conn, request.query.as_deref())?),
        "suggestTakeGroups" => {
            let file_ids = request.file_ids.as_deref().ok_or_else(|| anyhow::anyhow!("File IDs required"))?;
            let groups = super::catalog_take_suggestions::discover(conn, file_ids, request.limit.unwrap_or(20))?;
            if groups.is_empty() {
                result.message = Some("No related takes found among files with current visual embeddings and capture dates.".into());
            }
            result.suggested_take_groups = Some(groups);
        }
        "takeGroup" => {
            let id = event_id(request)?;
            group(conn, id, result)?;
        }
        "saveEvent" => {
            let mut event = request.event.clone().ok_or_else(|| anyhow::anyhow!("Event required"))?;
            event.title = event.title.trim().to_string();
            event.goal = event.goal.trim().to_string();
            event.file_ids = event.file_ids.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
            ensure!(!event.id.is_empty() && event.id.chars().count() <= 200
                && !event.title.is_empty() && event.title.chars().count() <= 200
                && !event.goal.is_empty() && event.goal.chars().count() <= 500
                && (2..=100).contains(&event.file_ids.len()) && event.file_ids.iter().all(|id| *id > 0), "Invalid event");
            event.user_edited = true;
            let tx = conn.transaction()?;
            let before = snapshot(&tx, &event.id)?;
            for file_id in &event.file_ids {
                ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE id=?1)", [file_id], |r| r.get::<_, bool>(0))?, "Unknown file");
            }
            tx.execute("INSERT INTO catalog_events(id,title,goal,user_edited) VALUES(?1,?2,?3,1) ON CONFLICT(id) DO UPDATE SET title=excluded.title,goal=excluded.goal,user_edited=1", params![event.id,event.title,event.goal])?;
            tx.execute("DELETE FROM catalog_event_files WHERE event_id=?1", [&event.id])?;
            for file_id in &event.file_ids {
                tx.execute("INSERT INTO catalog_event_files(event_id,file_id) VALUES(?1,?2)", params![event.id,file_id])?;
            }
            let after = snapshot(&tx, &event.id)?;
            journal(&tx, "event", &event.id, None, before.as_ref(), after.as_ref())?;
            tx.commit()?;
            group(conn, &event.id, result)?;
        }
        "deleteEvent" => {
            let id = event_id(request)?;
            let tx = conn.transaction()?;
            let before = snapshot(&tx, id)?.ok_or_else(|| anyhow::anyhow!("Event no longer available"))?;
            tx.execute("DELETE FROM catalog_events WHERE id=?1", [id])?;
            journal(&tx, "event", id, None, Some(&before), None)?;
            tx.commit()?;
            result.events = Some(list(conn, None)?);
        }
        "undoEventEdit" => {
            let id = event_id(request)?;
            let tx = conn.transaction()?;
            let (operation_id, before_json, after_json) = correction(&tx, "event", id, None)?;
            let before: Option<Snapshot> = serde_json::from_str(&before_json)?;
            let after: Option<Snapshot> = serde_json::from_str(&after_json)?;
            ensure!(snapshot(&tx, id)? == after, "Event changed since edit");
            tx.execute("DELETE FROM catalog_events WHERE id=?1", [id])?;
            if let Some(before) = before { restore(&tx, &before)?; }
            tx.execute("UPDATE catalog_operations SET state='undone' WHERE id=?1", [operation_id])?;
            tx.commit()?;
            result.events = Some(list(conn, None)?);
        }
        "setTakeFeedback" => {
            let feedback = request.take_feedback.as_ref().ok_or_else(|| anyhow::anyhow!("Feedback required"))?;
            ensure!(feedback.file_id > 0 && feedback.outcome_score.is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v)), "Invalid feedback");
            let tx = conn.transaction()?;
            ensure!(member(&tx, &feedback.event_id, feedback.file_id)?, "Take is not in this event");
            let before = score(&tx, &feedback.event_id, feedback.file_id)?;
            let source_revision = revision(&tx, feedback.file_id)?;
            let quality = before.as_ref().and_then(|score| if !score.stale && score.source_revision == source_revision { score.quality_score } else { None });
            let after = Score { event_id: feedback.event_id.clone(), file_id: feedback.file_id,
                outcome_score: feedback.outcome_score, quality_score: quality,
                confidence: 1.0, explanation: "Reviewed by you".into(), source_revision,
                model_version: "user".into(), preferred: feedback.preferred, stale: false };
            upsert(&tx, &after)?;
            journal(&tx, "take", &feedback.event_id, Some(feedback.file_id), before.as_ref(), Some(&after))?;
            tx.commit()?;
            group(conn, &feedback.event_id, result)?;
        }
        "undoTakeFeedback" => {
            let id = event_id(request)?;
            let file_id = request.file_id.ok_or_else(|| anyhow::anyhow!("File required"))?;
            let tx = conn.transaction()?;
            let (operation_id, before_json, after_json) = correction(&tx, "take", id, Some(file_id))?;
            let before: Option<Score> = serde_json::from_str(&before_json)?;
            let after: Option<Score> = serde_json::from_str(&after_json)?;
            ensure!(score(&tx, id, file_id)? == after, "Take changed since edit");
            tx.execute("DELETE FROM catalog_take_scores WHERE event_id=?1 AND file_id=?2", params![id,file_id])?;
            if let Some(before) = before { upsert(&tx, &before)?; }
            tx.execute("UPDATE catalog_operations SET state='undone' WHERE id=?1", [operation_id])?;
            tx.commit()?;
            group(conn, id, result)?;
        }
        _ => bail!("Unsupported take action"),
    }
    Ok(())
}

fn event_id(request: &CatalogRequest) -> Result<&str> {
    let id = request.event_id.as_deref().ok_or_else(|| anyhow::anyhow!("Event required"))?;
    ensure!(!id.is_empty() && id.chars().count() <= 200, "Invalid event ID");
    Ok(id)
}

fn list(conn: &Connection, query: Option<&str>) -> Result<Vec<CatalogEvent>> {
    let query = query.unwrap_or("").trim();
    ensure!(query.chars().count() <= 200, "Search too long");
    let mut stmt = conn.prepare("SELECT id FROM catalog_events WHERE ?1='' OR instr(lower(title),lower(?1))>0 OR instr(lower(goal),lower(?1))>0 ORDER BY title,id LIMIT 100")?;
    let ids = stmt.query_map([query], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter().map(|id| event(conn, id)?.ok_or_else(|| anyhow::anyhow!("Event disappeared"))).collect()
}

fn event(conn: &Connection, id: &str) -> Result<Option<CatalogEvent>> {
    let row = conn.query_row("SELECT title,goal,user_edited FROM catalog_events WHERE id=?1", [id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,bool>(2)?))).optional()?;
    let Some((title,goal,user_edited)) = row else { return Ok(None) };
    let mut stmt = conn.prepare("SELECT file_id FROM catalog_event_files WHERE event_id=?1 ORDER BY file_id")?;
    let file_ids = stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(CatalogEvent { id: id.into(), title, goal, file_ids, user_edited }))
}

fn group(conn: &Connection, id: &str, result: &mut CatalogResponse) -> Result<()> {
    let event = event(conn,id)?.ok_or_else(|| anyhow::anyhow!("Event no longer available"))?;
    let mut stmt = conn.prepare("SELECT f.id,f.path_text,s.outcome_score,s.quality_score,s.confidence,s.explanation,s.source_revision,s.model_version,s.preferred,s.stale FROM catalog_event_files ef JOIN files f ON f.id=ef.file_id LEFT JOIN catalog_take_scores s ON s.event_id=ef.event_id AND s.file_id=ef.file_id WHERE ef.event_id=?1 ORDER BY f.id")?;
    let takes = stmt.query_map([id], |r| {
        let file_id = r.get(0)?;
        let source_revision: Option<String> = r.get(6)?;
        let stored_stale: Option<bool> = r.get(9)?;
        let current = revision(conn,file_id).ok();
        Ok(CatalogTake { event_id:id.into(), file_id, path:r.get(1)?, outcome_score:r.get(2)?, quality_score:r.get(3)?, confidence:r.get(4)?, explanation:r.get(5)?, source_revision:source_revision.clone(), model_version:r.get(7)?, preferred:r.get::<_,Option<bool>>(8)?.unwrap_or(false), stale:stored_stale.unwrap_or(false) || source_revision.is_some_and(|s| Some(s) != current) })
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    result.recommendation = Some(recommend(&event,&takes));
    result.events = Some(vec![event]);
    result.takes = Some(takes);
    Ok(())
}

fn recommend(event: &CatalogEvent, takes: &[CatalogTake]) -> CatalogTakeRecommendation {
    let answer = |status: &str, file_ids: Vec<i64>, reason: &str| CatalogTakeRecommendation { event_id:event.id.clone(), status:status.into(), file_ids, reason:reason.into() };
    if event.goal.is_empty() || takes.len() < 2 { return answer("insufficient",vec![],"Describe a desired outcome and include at least two takes.") }
    let preferred: Vec<_> = takes.iter().filter(|t| t.preferred && !t.stale).map(|t| t.file_id).collect();
    if preferred.len() == 1 { return answer("preferred",preferred,"Marked as preferred by you.") }
    if preferred.len() > 1 { return answer("tie",preferred,"More than one take is marked preferred.") }
    if !takes.iter().all(|t| !t.stale && t.outcome_score.is_some() && t.confidence.unwrap_or(0.0) >= 0.65) {
        return answer("insufficient",vec![],"Some takes lack current outcome evidence; review them before choosing a winner.");
    }
    let best = takes.iter().filter_map(|t|t.outcome_score).fold(0.0_f64,f64::max);
    if best < 0.5 { return answer("insufficient",vec![],"No take has evidence of the desired outcome.") }
    let leaders: Vec<_> = takes.iter().filter(|t|t.outcome_score.unwrap_or(0.0) >= best-0.05).collect();
    if leaders.len() == 1 { return answer("ranked",vec![leaders[0].file_id],"Best supported desired outcome; technical quality considered separately.") }
    if leaders.iter().all(|t|t.quality_score.is_some()) {
        let mut ranked = leaders.clone();
        ranked.sort_by(|a,b|b.quality_score.unwrap_or(0.0).total_cmp(&a.quality_score.unwrap_or(0.0)));
        if ranked[0].quality_score.unwrap_or(0.0)-ranked[1].quality_score.unwrap_or(0.0)>=0.1 {
            return answer("ranked",vec![ranked[0].file_id],"Desired outcomes tie; this take has better technical quality.");
        }
    }
    answer("tie",leaders.iter().map(|t|t.file_id).collect(),"The supported outcomes are too close to choose one take.")
}

fn member(conn: &Connection, event_id: &str, file_id: i64) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM catalog_event_files WHERE event_id=?1 AND file_id=?2)",params![event_id,file_id],|r|r.get(0))?)
}

fn revision(conn: &Connection, file_id: i64) -> Result<String> {
    let (size,modified):(i64,Option<f64>) = conn.query_row("SELECT size_bytes,modified_at FROM files WHERE id=?1",[file_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    Ok(format!("{}:{}",size,modified.map(|m|m.to_bits().to_string()).unwrap_or_else(||"unknown".into())))
}

fn score(conn: &Connection, event_id: &str, file_id: i64) -> Result<Option<Score>> {
    Ok(conn.query_row("SELECT outcome_score,quality_score,confidence,explanation,source_revision,model_version,preferred,stale FROM catalog_take_scores WHERE event_id=?1 AND file_id=?2", params![event_id,file_id], |r| Ok(Score {event_id:event_id.into(),file_id,outcome_score:r.get(0)?,quality_score:r.get(1)?,confidence:r.get(2)?,explanation:r.get(3)?,source_revision:r.get(4)?,model_version:r.get(5)?,preferred:r.get(6)?,stale:r.get(7)?})).optional()?)
}

fn snapshot(conn: &Connection, event_id: &str) -> Result<Option<Snapshot>> {
    let Some(event) = event(conn,event_id)? else { return Ok(None) };
    let scores = event.file_ids.iter().filter_map(|id|score(conn,event_id,*id).transpose()).collect::<Result<Vec<_>>>()?;
    Ok(Some(Snapshot{event,scores}))
}

fn restore(conn: &Connection, snapshot: &Snapshot) -> Result<()> {
    let event = &snapshot.event;
    conn.execute("INSERT INTO catalog_events(id,title,goal,user_edited) VALUES(?1,?2,?3,?4)",params![event.id,event.title,event.goal,event.user_edited])?;
    for file_id in &event.file_ids { conn.execute("INSERT INTO catalog_event_files(event_id,file_id) VALUES(?1,?2)",params![event.id,file_id])?; }
    for score in &snapshot.scores { upsert(conn,score)?; }
    Ok(())
}

fn upsert(conn: &Connection, score: &Score) -> Result<()> {
    conn.execute("INSERT INTO catalog_take_scores(event_id,file_id,outcome_score,quality_score,confidence,explanation,source_revision,model_version,preferred,stale) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(event_id,file_id) DO UPDATE SET outcome_score=excluded.outcome_score,quality_score=excluded.quality_score,confidence=excluded.confidence,explanation=excluded.explanation,source_revision=excluded.source_revision,model_version=excluded.model_version,preferred=excluded.preferred,stale=excluded.stale",params![score.event_id,score.file_id,score.outcome_score,score.quality_score,score.confidence,score.explanation,score.source_revision,score.model_version,score.preferred,score.stale])?;
    Ok(())
}

fn journal<T: Serialize>(conn: &Connection, kind: &str, event_id: &str, file_id: Option<i64>, before: Option<&T>, after: Option<&T>) -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
    let before_json = serde_json::to_string(&before)?;
    let after_json = serde_json::to_string(&after)?;
    let plan = serde_json::json!({"eventID":event_id,"fileID":file_id.map(|id|id.to_string()).unwrap_or_default()}).to_string();
    conn.execute("INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,file_id,kind,before_json,after_json,now])?;
    conn.execute("INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?1,?2,?3,'completed',?4)",params![id,plan,before_json,now])?;
    Ok(())
}

fn correction(conn: &Transaction<'_>, kind: &str, event_id: &str, file_id: Option<i64>) -> Result<(String,String,String)> {
    let file_id = file_id.map(|id|id.to_string()).unwrap_or_default();
    Ok(conn.query_row("SELECT o.id,c.before_json,c.after_json FROM catalog_operations o JOIN catalog_corrections c ON c.id=o.id WHERE c.kind=?1 AND o.state='completed' AND json_extract(o.plan_json,'$.eventID')=?2 AND json_extract(o.plan_json,'$.fileID')=?3 ORDER BY o.rowid DESC LIMIT 1", params![kind,event_id,file_id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        for id in 1..=2 {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(?1,?2,?3,100,10,0,'video','mov')",params![id,format!("/internal/{id}.mov"),id]).unwrap();
        }
        conn
    }

    fn request(value: serde_json::Value) -> CatalogRequest {
        serde_json::from_value(value).unwrap()
    }

    fn run(conn: &mut Connection, value: serde_json::Value) -> CatalogResponse {
        crate::commands::catalog::execute(conn,&request(value)).unwrap()
    }

    #[test]
    fn reviewed_outcome_precedes_quality_and_undo_restores_abstention() {
        let mut conn = fixture();
        let save = run(&mut conn,serde_json::json!({"requestID":"save","action":"saveEvent","event":{"id":"game","title":"Baseball","goal":"Alex gets a hit","fileIDs":[1,2],"userEdited":false}}));
        assert_eq!(save.recommendation.unwrap().status,"insufficient");
        let source_revision = revision(&conn,1).unwrap();
        conn.execute("INSERT INTO catalog_take_scores(event_id,file_id,outcome_score,quality_score,confidence,explanation,source_revision,model_version,preferred) VALUES('game',1,0.1,0.99,0.9,'miss',?1,'fixture',0)",[source_revision]).unwrap();
        let feedback = run(&mut conn,serde_json::json!({"requestID":"review","action":"setTakeFeedback","takeFeedback":{"eventID":"game","fileID":2,"outcomeScore":1.0,"preferred":false}}));
        assert_eq!(feedback.recommendation.unwrap().file_ids,vec![2]);
        let undo = run(&mut conn,serde_json::json!({"requestID":"undo","action":"undoTakeFeedback","eventID":"game","fileID":2}));
        assert_eq!(undo.recommendation.unwrap().status,"insufficient");
        let delete = run(&mut conn,serde_json::json!({"requestID":"delete","action":"deleteEvent","eventID":"game"}));
        assert!(delete.events.unwrap().is_empty());
        let restored = run(&mut conn,serde_json::json!({"requestID":"restore","action":"undoEventEdit","eventID":"game"}));
        assert_eq!(restored.events.unwrap()[0].file_ids,vec![1,2]);
        conn.execute("UPDATE catalog_take_scores SET quality_score=0.9 WHERE event_id='game' AND file_id=1",[]).unwrap();
        conn.execute("UPDATE files SET size_bytes=200 WHERE id=1",[]).unwrap();
        let reviewed_again = run(&mut conn,serde_json::json!({"requestID":"recheck","action":"setTakeFeedback","takeFeedback":{"eventID":"game","fileID":1,"outcomeScore":0.0,"preferred":false}}));
        assert!(reviewed_again.takes.unwrap().iter().find(|take|take.file_id==1).unwrap().quality_score.is_none());
    }
}
