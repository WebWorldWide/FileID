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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogEvent {
    pub id: String,
    pub title: String,
    pub goal: String,
    #[serde(rename = "fileIDs")]
    pub file_ids: Vec<i64>,
    pub user_edited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTakeFeedback {
    #[serde(rename = "eventID")]
    pub event_id: String,
    #[serde(rename = "fileID")]
    pub file_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_score: Option<f64>,
    pub preferred: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTake {
    #[serde(rename = "eventID")]
    pub event_id: String,
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    pub preferred: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTakeRecommendation {
    #[serde(rename = "eventID")]
    pub event_id: String,
    pub status: String,
    #[serde(rename = "fileIDs")]
    pub file_ids: Vec<i64>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTakeGroupMember {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTakeGroupSuggestion {
    pub members: Vec<CatalogTakeGroupMember>,
    pub similarity: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CatalogTakeBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTakeShot {
    #[serde(rename = "primaryFileID")]
    pub primary_file_id: i64,
    #[serde(rename = "fileIDs")]
    pub file_ids: Vec<i64>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTakeAutoGroup {
    pub group_key: String,
    pub kind: String,
    pub status: String,
    pub captured_at: f64,
    pub similarity: f64,
    pub shots: Vec<CatalogTakeShot>,
}

impl CatalogTakeAutoGroup {
    pub fn file_ids(&self) -> Vec<i64> {
        self.shots.iter().flat_map(|s| s.file_ids.iter().copied()).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTakeQuality {
    #[serde(rename = "primaryFileID")]
    pub primary_file_id: i64,
    #[serde(rename = "fileIDs")]
    pub file_ids: Vec<i64>,
    pub path: String,
    pub score: f64,
    pub rank: usize,
    pub recommended: bool,
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharpness: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_blur: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eyes_closed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_cut_off: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stability: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<CatalogTakeBox>,
    pub faces: Vec<CatalogTakeBox>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTakeGoalRanking {
    pub goal: String,
    pub status: String,
    pub order: Vec<i64>,
    #[serde(rename = "metFileIDs")]
    pub met_file_ids: Vec<i64>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogTakeDecision {
    pub group_key: String,
    pub decision: String,
    #[serde(rename = "fileIDs")]
    pub file_ids: Vec<i64>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<CatalogEvent>,
    #[serde(rename = "eventID", default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_feedback: Option<CatalogTakeFeedback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_decision: Option<CatalogTakeDecision>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<CatalogEvent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takes: Option<Vec<CatalogTake>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<CatalogTakeRecommendation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_take_groups: Option<Vec<CatalogTakeGroupSuggestion>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_groups: Option<Vec<CatalogTakeAutoGroup>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_qualities: Option<Vec<CatalogTakeQuality>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_goal_ranking: Option<Box<CatalogTakeGoalRanking>>,
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
