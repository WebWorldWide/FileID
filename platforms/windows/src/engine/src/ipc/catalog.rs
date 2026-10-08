use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogChapter {
    pub id: String,
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub title: String,
    pub summary: String,
    pub source_revision: String,
    pub model_version: String,
    pub confidence: f64,
    pub user_edited: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogHit {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub path: String,
    pub kind: String,
    pub text: String,
    #[serde(rename = "evidenceID", default, skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogJob {
    pub id: String,
    pub kind: String,
    #[serde(rename = "fileIDs")]
    pub file_ids: Vec<i64>,
    pub state: String,
    pub progress: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_mode: Option<String>,
    #[serde(rename = "requestID")]
    pub request_id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(rename = "fileID", default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter: Option<CatalogChapter>,
    #[serde(rename = "chapterID", default, skip_serializing_if = "Option::is_none")]
    pub chapter_id: Option<String>,
    #[serde(rename = "jobID", default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(rename = "fileIDs", default, skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_vector: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogResponse {
    #[serde(rename = "requestID")]
    pub request_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub hits: Vec<CatalogHit>,
    pub chapters: Vec<CatalogChapter>,
    pub jobs: Vec<CatalogJob>,
}

#[cfg(test)]
mod tests {
    use super::CatalogRequest;

    #[test]
    fn moment_depth_round_trips_and_legacy_requests_keep_the_default() {
        let request: CatalogRequest = serde_json::from_str(
            r#"{"requestID":"moments","action":"enqueueTimeline","fileIDs":[1],"timelineMode":"moments"}"#,
        ).unwrap();
        assert_eq!(request.timeline_mode.as_deref(), Some("moments"));
        let wire = serde_json::to_value(request).unwrap();
        assert_eq!(wire["timelineMode"], "moments");
        let legacy: CatalogRequest = serde_json::from_str(
            r#"{"requestID":"old","action":"enqueueTimeline","fileIDs":[1]}"#,
        ).unwrap();
        assert!(legacy.timeline_mode.is_none());
        assert!(serde_json::to_value(legacy).unwrap().get("timelineMode").is_none());
    }
}
