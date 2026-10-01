use std::sync::Arc;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use crate::ipc::{ChatMessage, ChatRequest, ChatResponse, EventPayload, IpcEvent, Wrap};
use crate::ipc::sink::Sink;

pub async fn handle(sink: Sink, database: Option<Arc<Mutex<Connection>>>, request: ChatRequest) {
    let id = request.request_id.clone();
    let conversation = request.conversation_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let Some(database) = database else { bail!("The catalog database is unavailable") };
        let result = execute(&mut database.lock(), &request);
        result
    }).await;
    let response = match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => failed(id,conversation,error.to_string()),
        Err(error) => failed(id,conversation,error.to_string()),
    };
    sink.send(IpcEvent { t: chrono::Utc::now(), payload: EventPayload::ChatResponse(Wrap::new(response)) }).await;
}

fn failed(request_id: String, conversation_id: String, message: String) -> ChatResponse {
    ChatResponse { request_id,conversation_id,status:"error".into(),message,messages:vec![],hits:vec![] }
}

pub fn execute(conn: &mut Connection, request: &ChatRequest) -> Result<ChatResponse> {
    if request.request_id.is_empty() || request.request_id.chars().count()>200 || request.conversation_id.is_empty() || request.conversation_id.chars().count()>200 { bail!("Invalid conversation request") }
    let mut response = ChatResponse { request_id:request.request_id.clone(),conversation_id:request.conversation_id.clone(),status:"completed".into(),message:String::new(),messages:vec![],hits:vec![] };
    match request.action.as_str() {
        "history" => response.message = "Local conversation history.".into(),
        "clear" => {
            conn.execute("DELETE FROM catalog_chat WHERE conversation_id=?1",[&request.conversation_id])?;
            response.message = "Conversation deleted.".into();
        }
        "cancel" => { response.status="cancelled".into();response.message="No model response is running on this adapter.".into(); }
        "send" => {
            let text = request.text.as_deref().unwrap_or_default();
            if text.trim().is_empty() || text.chars().count()>2000 { bail!("A message of at most 2000 characters is required") }
            let previous = {
                let mut statement = conn.prepare("SELECT text FROM (SELECT rowid,text FROM catalog_chat WHERE conversation_id=?1 AND role='user' ORDER BY rowid DESC LIMIT 20) ORDER BY rowid")?;
                let messages = statement.query_map([&request.conversation_id], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                messages.iter().fold(None, |previous,text| Some(super::chat_search::SearchPlan::resolve(text,previous.as_ref())))
            };
            let plan = super::chat_search::SearchPlan::resolve(text,previous.as_ref());
            response.hits = super::catalog::search(conn,&plan.query,&plan.kinds)?;
            let scope = if plan.query.is_empty() { "all catalog files".to_owned() } else { format!("“{}”",plan.query) };
            let filter = if plan.kinds.is_empty() { String::new() } else { format!(" ({})",plan.kinds.join(", ")) };
            response.message = if plan.query.is_empty() && plan.kinds.is_empty() {
                "Add a subject or a media type such as videos or photos. No search was run.".into()
            } else if response.hits.is_empty() {
                format!("No keyword matches for {scope}{filter}. Try names or a few descriptive terms. Unanalyzed files may still contain the requested event.")
            } else { format!("Found {} file or evidence matches for {scope}{filter}. Sampled-frame descriptions remain unverified.",response.hits.len()) };
            if request.use_model == Some(true) { response.message.push_str(" Model summaries are not yet available on this adapter; no download was started."); }
            let tx = conn.transaction()?;
            for (role,content) in [("user",text),("assistant",&response.message)] {
                tx.execute("INSERT INTO catalog_chat(id,conversation_id,role,text,created_at) VALUES(?1,?2,?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),request.conversation_id,role,content,chrono::Utc::now().timestamp_millis() as f64 /1000.0])?;
            }
            tx.commit()?;
        }
        _ => bail!("Unknown conversation action"),
    }
    let mut statement = conn.prepare("SELECT id,role,text,created_at FROM (SELECT rowid,* FROM catalog_chat WHERE conversation_id=?1 ORDER BY rowid DESC LIMIT 100) ORDER BY rowid")?;
    response.messages=statement.query_map([&request.conversation_id],|r| Ok(ChatMessage {id:r.get(0)?,role:r.get(1)?,text:r.get(2)?,created_at:r.get(3)?}))?.collect::<rusqlite::Result<_>>()?;
    Ok(response)
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_search_and_clear_preserve_catalog_files() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(1,'/offline/birthday.mov',1,100,0,'video','mov','Birthday gift opening')",[]).unwrap();
        let mut request = ChatRequest { request_id:"r".into(),conversation_id:"c".into(),action:"send".into(),text:Some("Please find my birthday videos".into()),use_model:Some(true) };
        let result = execute(&mut conn,&request).unwrap();
        assert_eq!(result.hits[0].file_id,1);
        assert_eq!(result.messages.len(),2);
        assert!(result.message.contains("not yet available"));
        request.action="history".into();
        assert_eq!(execute(&mut conn,&request).unwrap().messages.len(),2);
        request.conversation_id="another".into();
        assert!(execute(&mut conn,&request).unwrap().messages.is_empty());
        request.conversation_id="c".into();request.action="clear".into();
        assert!(execute(&mut conn,&request).unwrap().messages.is_empty());
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM files",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }

    #[test]
    fn refinement_filters_files_and_evidence_before_limits() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        for id in 1..=150 {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(?1,?2,?1,100,0,?3,'mov','Birthday gift opening')",params![id,format!("/offline/birthday-{id}.mov"),if id == 1 { "video" } else { "image" }]).unwrap();
        }
        conn.execute("INSERT INTO catalog_passages(id,file_id,text,source_revision,model_version,confidence,stale) VALUES('v',1,'home run','test','test',1,0),('p',2,'home run','test','test',1,0)",[]).unwrap();
        let videos = vec!["video".to_owned()];
        assert_eq!(super::super::catalog::search(&conn,"birthday",&videos).unwrap().iter().map(|hit|hit.file_id).collect::<Vec<_>>(),vec![1]);
        assert_eq!(super::super::catalog::search(&conn,"",&videos).unwrap().iter().map(|hit|hit.file_id).collect::<Vec<_>>(),vec![1]);
        assert_eq!(super::super::catalog::search(&conn,"home run",&videos).unwrap()[0].evidence_id.as_deref(),Some("v"));
        for text in ["find birthday","only videos"] {
            let request = ChatRequest {request_id:uuid::Uuid::new_v4().to_string(),conversation_id:"c".into(),action:"send".into(),text:Some(text.into()),use_model:Some(false)};
            let result = execute(&mut conn,&request).unwrap();
            if text == "only videos" { assert_eq!(result.hits.iter().map(|hit|hit.file_id).collect::<Vec<_>>(),vec![1]); assert!(result.message.contains("birthday")); }
        }
    }

    #[test]
    fn invalid_requests_cannot_write_history_or_change_files() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        for text in [String::new(),"x".repeat(2001)] {
            let request = ChatRequest {request_id:"r".into(),conversation_id:"c".into(),action:"send".into(),text:Some(text),use_model:None};
            assert!(execute(&mut conn,&request).is_err());
        }
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM catalog_chat",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(super::super::chat_search::SearchPlan::resolve("Please show me my birthday videos",None).query,"birthday");
    }
}
