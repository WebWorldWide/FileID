use serde::{Deserialize, Serialize};
use super::CatalogHit;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatMessage {
    pub id: String,
    pub role: String,
    pub text: String,
    pub created_at: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(rename = "fileIDs", default, skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<i64>>,
    #[serde(rename = "personIDs", default, skip_serializing_if = "Option::is_none")]
    pub person_ids: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_root: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatRequest {
    #[serde(rename = "requestID")]
    pub request_id: String,
    #[serde(rename = "conversationID")]
    pub conversation_id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_model: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ChatContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_vector: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatRename {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub new_name: String,
}

/// A proposed FileID action carried by a chat reply. Clients run it only
/// through the existing command it maps to, after a confirmation card when
/// `requires_confirmation` is set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatAction {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub requires_confirmation: bool,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub examples: Option<Vec<String>>,
    #[serde(rename = "fileIDs", default, skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<i64>>,
    #[serde(rename = "keeperFileIDs", default, skip_serializing_if = "Option::is_none")]
    pub keeper_file_ids: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(rename = "personID", default, skip_serializing_if = "Option::is_none")]
    pub person_id: Option<i64>,
    #[serde(rename = "personIDs", default, skip_serializing_if = "Option::is_none")]
    pub person_ids: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renames: Option<Vec<ChatRename>>,
}

pub const CHAT_ACTION_KINDS: [&str; 17] = [
    "scanFolder", "rescan", "describeFiles", "stopJob", "findDuplicates", "moveDuplicatesToTrash",
    "suggestFolderLayout", "applyFolderLayout", "undoLastMove", "renameToSuggested", "namePerson",
    "mergePeople", "findMoments", "exportFiles", "openPage", "search", "changeSetting",
];
pub const CHAT_PAGES: [&str; 9] = ["library", "people", "cleanup", "deepAnalyze", "restructure", "settings", "search", "tools", "chat"];
pub const CHAT_JOBS: [&str; 5] = ["scan", "describe", "organize", "moments", "all"];
pub const CHAT_SETTINGS: [&str; 3] = ["cleanupAutoTag", "backgroundMotion", "chatSummaries"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatResponse {
    #[serde(rename = "requestID")]
    pub request_id: String,
    #[serde(rename = "conversationID")]
    pub conversation_id: String,
    pub status: String,
    pub message: String,
    pub messages: Vec<ChatMessage>,
    pub hits: Vec<CatalogHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<ChatAction>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRequestPayload { pub request: ChatRequest }
