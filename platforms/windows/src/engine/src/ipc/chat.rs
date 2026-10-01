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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRequestPayload { pub request: ChatRequest }
