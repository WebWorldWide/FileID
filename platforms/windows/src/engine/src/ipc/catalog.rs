use super::CatalogTakeAutoGroup;

impl CatalogTakeAutoGroup {
    pub fn file_ids(&self) -> Vec<i64> {
        self.shots.iter().flat_map(|s| s.file_ids.iter().copied()).collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::ipc::CatalogRequest;

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
