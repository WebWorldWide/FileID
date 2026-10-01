use std::sync::Arc;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use crate::ipc::{CatalogRequest, ChatMessage, ChatRequest, ChatResponse, EventPayload, IpcEvent, Wrap};
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
            let query = query(text);
            if !query.is_empty() {
                response.hits = super::catalog::execute(conn,&CatalogRequest {request_id:request.request_id.clone(),action:"search".into(),query:Some(query.clone()),file_id:None,chapter:None,chapter_id:None,job_id:None,file_ids:None})?.hits;
            }
            response.message = if response.hits.is_empty() {
                "No keyword matches in the current catalog. Try names or a few descriptive terms. Unanalyzed files may still contain the requested event.".into()
            } else { format!("Found {} file or evidence matches for “{}”. Sampled-frame descriptions remain unverified.",response.hits.len(),query) };
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

fn query(text: &str) -> String {
    let excluded = ["find","show","me","please","search","for","the","a","an","where","of","my","files","videos","photos","pictures","documents","clips","with","in","all"];
    text.split_whitespace().filter(|part| !excluded.contains(&part.to_lowercase().as_str())).collect::<Vec<_>>().join(" ")
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
    fn invalid_requests_cannot_write_history_or_change_files() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        for text in [String::new(),"x".repeat(2001)] {
            let request = ChatRequest {request_id:"r".into(),conversation_id:"c".into(),action:"send".into(),text:Some(text),use_model:None};
            assert!(execute(&mut conn,&request).is_err());
        }
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM catalog_chat",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(query("Please show me my birthday videos"),"birthday");
    }
}
