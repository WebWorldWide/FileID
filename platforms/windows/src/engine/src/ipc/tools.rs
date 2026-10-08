use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRecipe {
    pub kind: String,
    pub format: String,
    pub max_dimension: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_upscale: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCapability {
    pub id: String,
    pub available: bool,
    pub input_formats: Vec<String>,
    pub output_formats: Vec<String>,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolOutput {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub source_path: String,
    pub output_path: String,
    pub state: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRequest {
    #[serde(rename = "requestID")]
    pub request_id: String,
    pub action: String,
    #[serde(rename = "fileIDs", default, skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ToolRecipe>,
    #[serde(rename = "operationID", default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_bookmark: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolResponse {
    #[serde(rename = "requestID")]
    pub request_id: String,
    pub status: String,
    pub message: String,
    #[serde(rename = "operationID", default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub outputs: Vec<ToolOutput>,
    pub capabilities: Vec<ToolCapability>,
}
