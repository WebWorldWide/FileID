#[derive(Default)]
pub(super) struct DeepTransportStatus {
    reason: Option<String>,
}

impl DeepTransportStatus {
    pub(super) fn failed(&mut self, analysis_failure: Option<&str>, reason: &str) -> String {
        let reason = self.reason.get_or_insert_with(|| reason.to_owned());
        with_failure(
            analysis_failure,
            &format!("Engine unavailable; analysis interrupted: {reason}"),
        )
    }

    pub(super) fn exited(&self, analysis_failure: Option<&str>) -> String {
        let mut status =
            "Engine exited; analysis interrupted. Indexed results remain stored.".to_owned();
        if let Some(reason) = &self.reason {
            status.push_str(&format!("\nTransport failure: {reason}"));
        }
        with_failure(analysis_failure, &status)
    }

    pub(super) fn cancellation_requested(analysis_failure: Option<&str>) -> String {
        with_failure(
            analysis_failure,
            "Cancellation requested; waiting for engine completion…",
        )
    }

    pub(super) fn cancellation_failed(analysis_failure: Option<&str>, reason: &str) -> String {
        with_failure(analysis_failure, &format!("Could not cancel: {reason}"))
    }
}

fn with_failure(analysis_failure: Option<&str>, status: &str) -> String {
    match analysis_failure {
        Some(failure) => format!("Failed / unavailable: {failure}\n{status}"),
        None => status.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_failure_survives_failed_then_exited_with_transport_explanation() {
        let mut status = DeepTransportStatus::default();
        let failure = "The llama.cpp runtime isn't usable for image analysis";
        let failed = status.failed(Some(failure), "engine output error: generated fixture EOF");
        assert!(failed.starts_with("Failed / unavailable"));
        assert!(failed.contains(failure));
        assert!(failed.contains("analysis interrupted"));
        assert!(failed.contains("generated fixture EOF"));
        let exited = status.exited(Some(failure));
        assert!(exited.contains(failure));
        assert!(exited.contains("Engine exited"));
        assert!(exited.contains("generated fixture EOF"));
        assert!(!exited.contains("Completed"));
    }

    #[test]
    fn cancellation_request_and_send_failure_do_not_confirm_cancellation() {
        let failure = "Generated fixture analysis failed";
        let request = DeepTransportStatus::cancellation_requested(Some(failure));
        assert!(request.contains(failure));
        assert!(request.contains("waiting for engine completion"));
        assert!(!request.contains("Cancelled"));
        let rejected = DeepTransportStatus::cancellation_failed(Some(failure), "generated pipe error");
        assert!(rejected.contains(failure));
        assert!(rejected.contains("Could not cancel: generated pipe error"));
        assert!(!rejected.contains("Cancelled"));
        let mut status = DeepTransportStatus::default();
        assert!(status.failed(Some(failure), "generated transport failure").contains(failure));
        assert!(status.exited(Some(failure)).contains(failure));
    }

    #[test]
    fn no_error_interruption_is_not_presented_as_success_or_cancellation() {
        let mut status = DeepTransportStatus::default();
        let failed = status.failed(None, "generated read error");
        assert!(failed.contains("analysis interrupted: generated read error"));
        assert!(!failed.contains("Failed / unavailable"));
        let exited = status.exited(None);
        assert!(exited.contains("generated read error"));
        assert!(!exited.contains("Completed"));
        assert!(!exited.contains("Cancelled"));
        assert_eq!(
            DeepTransportStatus::cancellation_requested(None),
            "Cancellation requested; waiting for engine completion…"
        );
        assert_eq!(
            DeepTransportStatus::cancellation_failed(None, "generated pipe error"),
            "Could not cancel: generated pipe error"
        );
        assert!(!DeepTransportStatus::default().exited(None).contains("Transport failure:"));
    }

    #[test]
    fn first_transport_reason_is_preserved_and_new_presentation_state_resets_it() {
        let mut status = DeepTransportStatus::default();
        status.failed(Some("first owned failure"), "first transport failure");
        let repeated = status.failed(Some("first owned failure"), "later transport failure");
        assert!(repeated.contains("first owned failure"));
        assert!(repeated.contains("first transport failure"));
        assert!(!repeated.contains("later transport failure"));
        status = DeepTransportStatus::default();
        let new_run = status.exited(None);
        assert!(!new_run.contains("first owned failure"));
        assert!(!new_run.contains("first transport failure"));
    }
}
