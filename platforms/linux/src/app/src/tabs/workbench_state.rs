use fileid_engine::ipc::{
    CatalogRequest, ChatRequest, ChatResponse, ClipTextEmbedding, EmbedTextQueryPayload,
    ToolCapability, ToolRecipe,
};
use fileid_engine::models::clip_space;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

pub fn request_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "gtk-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Default)]
pub struct Requests(HashMap<String, String>);
impl Requests {
    pub fn begin(&mut self, slot: &str, id: &str) {
        self.0.insert(slot.into(), id.into());
    }
    pub fn accept(&mut self, id: &str, terminal: bool) -> Option<String> {
        let slot = self
            .0
            .iter()
            .find(|(_, current)| current.as_str() == id)?
            .0
            .clone();
        if terminal {
            self.0.remove(&slot);
        }
        Some(slot)
    }
    pub fn busy(&self, slot: &str) -> bool {
        self.0.contains_key(slot)
    }
    pub fn forget(&mut self, slot: &str) {
        self.0.remove(slot);
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

#[derive(Default)]
pub struct ExportState {
    pub requests: Requests,
    pub capabilities: Vec<ToolCapability>,
    pub preview: Option<String>,
    pub history: Option<String>,
    pub revision: u64,
    pub preview_revision: u64,
    pub operation_action: Option<String>,
}
impl ExportState {
    pub fn invalidate(&mut self) {
        self.revision += 1;
        self.preview = None;
    }
    pub fn supports(&self, recipe: &ToolRecipe) -> bool {
        (1..=8192).contains(&recipe.max_dimension)
            && self.capabilities.iter().any(|c| {
                c.id == recipe.kind && c.available && c.output_formats.contains(&recipe.format)
            })
    }
    pub fn can_execute(&self) -> bool {
        self.preview.is_some()
            && self.preview_revision == self.revision
            && !self.requests.busy("operation")
    }
}

pub fn catalog_request(action: &str) -> CatalogRequest {
    CatalogRequest {
        timeline_mode: None,
        request_id: request_id(),
        action: action.into(),
        query: None,
        file_id: None,
        chapter: None,
        chapter_id: None,
        job_id: None,
        file_ids: None,
        search_mode: None,
        query_vector: None,
        embedding_model: None,
        limit: Some(100),
        result_scope: None,
        event: None,
        event_id: None,
        take_feedback: None,
    }
}

pub fn chat_matches(
    requests: &mut Requests,
    id: &str,
    conversation: &str,
    current: &str,
    status: &str,
) -> bool {
    conversation == current
        && requests
            .accept(id, !matches!(status, "retrieving" | "queued" | "streaming"))
            .is_some()
}

#[derive(Default)]
pub struct ChatState {
    pub display: Requests,
    pending_write: Option<ChatRequest>,
    deferred_clears: VecDeque<ChatRequest>,
}
impl ChatState {
    pub fn begin(&mut self, request: &ChatRequest) -> Result<bool, &'static str> {
        if request.action == "send"
            && (self.pending_write.is_some() || !self.deferred_clears.is_empty())
        {
            return Err("Your last message is still being saved. Wait a moment; History, Stop and changes to the conversation don't cancel it.");
        }
        self.display.begin("chat", &request.request_id);
        if request.action == "clear" && self.pending_write.is_some() {
            self.deferred_clears
                .retain(|r| r.conversation_id != request.conversation_id);
            self.deferred_clears.push_back(request.clone());
            return Ok(false);
        }
        if matches!(request.action.as_str(), "send" | "clear") {
            self.pending_write = Some(request.clone());
        }
        Ok(true)
    }
    pub fn finish(&mut self, response: &ChatResponse) -> Option<ChatRequest> {
        let pending = self.pending_write.as_ref()?;
        if pending.request_id != response.request_id
            || pending.conversation_id != response.conversation_id
            || matches!(
                response.status.as_str(),
                "retrieving" | "queued" | "streaming"
            )
        {
            return None;
        }
        self.pending_write = self.deferred_clears.pop_front();
        self.pending_write.clone()
    }
    pub fn interrupted(&mut self) {
        self.display.clear();
        self.pending_write = None;
        self.deferred_clears.clear();
    }
}

#[derive(Default)]
pub struct HybridSearch {
    pending: Option<EmbedTextQueryPayload>,
}
impl HybridSearch {
    pub fn begin(&mut self, query: &str) -> EmbedTextQueryPayload {
        let request = EmbedTextQueryPayload {
            query: query.into(),
            query_id: request_id(),
        };
        self.pending = Some(request.clone());
        request
    }
    pub fn cancel(&mut self) {
        self.pending = None;
    }
    pub fn finish(
        &mut self,
        response: &ClipTextEmbedding,
    ) -> Option<(CatalogRequest, Option<String>)> {
        let pending = self.pending.as_ref()?;
        if pending.query_id != response.query_id || pending.query != response.query {
            return None;
        }
        self.pending = None;
        let mut request = catalog_request("search");
        request.query = Some(response.query.clone());
        if !clip_space::valid(&response.embedding) {
            return Some((request, Some("Hybrid search isn't available: the Search by description model didn't return a usable result. Using keyword search; nothing was downloaded.".into())));
        }
        request.search_mode = Some("hybrid".into());
        request.query_vector = Some(response.embedding.clone());
        request.embedding_model = Some(clip_space::model_id().to_owned());
        Some((request, None))
    }
    pub fn timeout(&mut self, query_id: &str) -> Option<CatalogRequest> {
        if self.pending.as_ref()?.query_id != query_id {
            return None;
        }
        let pending = self.pending.take()?;
        let mut request = catalog_request("search");
        request.query = Some(pending.query);
        Some(request)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_slots_reject_late_search_but_keep_independent_jobs() {
        let mut r = Requests::default();
        r.begin("search", "old");
        r.begin("jobs", "jobs");
        r.begin("search", "new");
        assert!(r.accept("old", true).is_none());
        assert_eq!(r.accept("jobs", true).as_deref(), Some("jobs"));
        assert_eq!(r.accept("new", true).as_deref(), Some("search"));
        assert!(r.accept("new", true).is_none());
    }
    #[test]
    fn chat_rejects_other_conversations_and_cancelled_streams() {
        let mut r = Requests::default();
        r.begin("chat", "send");
        assert!(!chat_matches(
            &mut r,
            "send",
            "other",
            "current",
            "completed"
        ));
        assert!(chat_matches(
            &mut r,
            "send",
            "current",
            "current",
            "streaming"
        ));
        r.begin("chat", "cancel");
        assert!(!chat_matches(
            &mut r,
            "send",
            "current",
            "current",
            "completed"
        ));
        assert!(chat_matches(
            &mut r,
            "cancel",
            "current",
            "current",
            "cancelled"
        ));
    }
    #[test]
    fn exports_require_capability_and_unchanged_preview() {
        let mut s = ExportState::default();
        let recipe = ToolRecipe {
            kind: "photo".into(),
            format: "png".into(),
            max_dimension: 1024,
            allow_upscale: None,
            video_aspect_ratio: None,
            video_framing: None,
            video_stabilization: None,
        };
        assert!(!s.supports(&recipe));
        s.capabilities.push(ToolCapability {
            id: "photo".into(),
            available: true,
            input_formats: vec![],
            output_formats: vec!["png".into()],
            detail: String::new(),
        });
        assert!(s.supports(&recipe));
        s.preview = Some("operation".into());
        assert!(s.can_execute());
        s.requests.begin("operation", "execute");
        assert!(!s.can_execute());
        s.requests.clear();
        s.invalidate();
        assert!(!s.can_execute());
    }
    fn chat_request(id: &str, conversation: &str, action: &str) -> ChatRequest {
        ChatRequest {
            request_id: id.into(),
            conversation_id: conversation.into(),
            action: action.into(),
            text: Some("fixture subject".into()),
            use_model: Some(false),
            ..ChatRequest::default()
        }
    }
    #[test]
    fn chat_defers_clear_until_send_persistence_finishes_even_when_display_is_replaced() {
        let mut state = ChatState::default();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        let send = chat_request("send", "a", "send");
        let clear = chat_request("clear", "a", "clear");
        assert!(state.begin(&send).unwrap());
        assert!(!state.begin(&clear).unwrap());
        let history = chat_request("history", "a", "history");
        assert!(state.begin(&history).unwrap());
        let history_response = fileid_engine::commands::chat::execute(&mut conn, &history).unwrap();
        assert!(state.finish(&history_response).is_none());
        assert!(state.begin(&chat_request("overlap", "a", "send")).is_err());
        let response = fileid_engine::commands::chat::execute(&mut conn, &send).unwrap();
        assert_eq!(response.messages.len(), 2);
        let deferred = state.finish(&response).unwrap();
        assert_eq!(deferred.request_id, "clear");
        assert!(state
            .begin(&chat_request("overlap-clear", "a", "send"))
            .is_err());
        let cleared = fileid_engine::commands::chat::execute(&mut conn, &deferred).unwrap();
        assert!(cleared.messages.is_empty());
        assert!(state.finish(&cleared).is_none());
        assert!(fileid_engine::commands::chat::execute(&mut conn, &history)
            .unwrap()
            .messages
            .is_empty());
        assert!(state.begin(&chat_request("next", "a", "send")).unwrap());
    }
    #[test]
    fn chat_stop_history_and_conversation_changes_never_release_outstanding_send() {
        let mut state = ChatState::default();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        let send = chat_request("send", "a", "send");
        assert!(state.begin(&send).unwrap());
        for action in ["history", "cancel"] {
            let control = chat_request(action, "a", action);
            assert!(state.begin(&control).unwrap());
            let response = fileid_engine::commands::chat::execute(&mut conn, &control).unwrap();
            assert!(state.finish(&response).is_none());
            assert!(state.begin(&chat_request("overlap", "a", "send")).is_err());
        }
        state.display.clear();
        assert!(state
            .begin(&chat_request("different-conversation", "b", "send"))
            .is_err());
        let clear_b = chat_request("clear-b", "b", "clear");
        assert!(!state.begin(&clear_b).unwrap());
        let mut response = fileid_engine::commands::chat::execute(&mut conn, &send).unwrap();
        response.conversation_id = "b".into();
        assert!(state.finish(&response).is_none());
        response.conversation_id = "a".into();
        response.status = "streaming".into();
        assert!(state.finish(&response).is_none());
        response.status = "completed".into();
        assert_eq!(state.finish(&response).unwrap().request_id, "clear-b");
        let cleared = fileid_engine::commands::chat::execute(&mut conn, &clear_b).unwrap();
        state.finish(&cleared);
        assert!(state.begin(&chat_request("next", "b", "send")).unwrap());
    }
    #[test]
    fn hybrid_uses_exact_pinned_identity_and_rejects_late_query_or_invalid_vectors() {
        assert_eq!(clip_space::model_id(),"clip-vitb32-openai-v1:126bacbb4038a0820ca6582bd81f58d7892070deab8317b390e37d7632a53471");
        let mut search = HybridSearch::default();
        let old = search.begin("old");
        let current = search.begin("current");
        let mut vector = vec![0.0; 512];
        vector[0] = 1.0;
        let mut response = ClipTextEmbedding {
            query_id: old.query_id,
            query: "old".into(),
            embedding: vector.clone(),
        };
        assert!(search.finish(&response).is_none());
        response.query_id = current.query_id.clone();
        assert!(search.finish(&response).is_none());
        response.query = current.query;
        let (request, reason) = search.finish(&response).unwrap();
        assert!(reason.is_none());
        assert_eq!(request.search_mode.as_deref(), Some("hybrid"));
        assert_eq!(request.query_vector, Some(vector));
        assert_eq!(
            request.embedding_model,
            Some(clip_space::model_id().to_owned())
        );
        let command = fileid_engine::ipc::IpcCommand {
            id: "hybrid-fixture".into(),
            payload: fileid_engine::ipc::CommandPayload::CatalogRequest(Box::new(
                fileid_engine::ipc::CatalogRequestPayload { request },
            )),
        };
        let encoded = serde_json::to_value(&command).unwrap();
        assert_eq!(
            encoded["payload"]["catalogRequest"]["request"]["queryVector"]
                .as_array()
                .unwrap()
                .len(),
            512
        );
        assert_eq!(
            encoded["payload"]["catalogRequest"]["request"]["searchMode"],
            "hybrid"
        );
        let decoded: fileid_engine::ipc::IpcCommand = serde_json::from_value(encoded).unwrap();
        let fileid_engine::ipc::CommandPayload::CatalogRequest(payload) = decoded.payload else {
            panic!("Wrong typed hybrid command");
        };
        assert_eq!(
            payload.request.embedding_model,
            Some(clip_space::model_id().to_owned())
        );

        for invalid in [
            vec![],
            vec![0.0; 512],
            vec![1.0; 512],
            vec![f32::NAN; 512],
            vec![f32::INFINITY; 512],
            vec![1.0; 511],
        ] {
            let pending = search.begin("fallback");
            let (request, reason) = search
                .finish(&ClipTextEmbedding {
                    query_id: pending.query_id,
                    query: pending.query,
                    embedding: invalid,
                })
                .unwrap();
            assert!(request.query_vector.is_none());
            assert!(request.embedding_model.is_none());
            assert!(request.search_mode.is_none());
            assert!(reason.unwrap().contains("Using keyword search"));
        }
        let pending = search.begin("timeout");
        assert!(search.timeout("other").is_none());
        assert_eq!(
            search.timeout(&pending.query_id).unwrap().query.as_deref(),
            Some("timeout")
        );
        assert!(search.finish(&response).is_none());
    }
}
