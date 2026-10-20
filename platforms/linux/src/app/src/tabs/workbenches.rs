use super::workbench_state::*;
use crate::engine_client::EngineClient;
use fileid_engine::ipc::*;
use std::{cell::RefCell, rc::Rc};

pub(super) type Engine = Rc<RefCell<EngineClient>>;
/// Video and audio by the engine's own classifier, so every extension the
/// scan treats as media (MTS, M2TS, 3GP, MPG, Opus, AIFF, ...) plays in place.
pub(super) fn is_playable_media(path: &str) -> bool {
    use fileid_engine::pipeline::discovery::FileKind;
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(FileKind::from_extension(ext), FileKind::Video | FileKind::Audio))
}
pub fn catalog(engine: Engine) -> gtk::Widget {
    super::moments::catalog(engine)
}

pub(super) fn timeline_request(file_ids: Vec<i64>, mode: &str) -> CatalogRequest {
    let mut r = catalog_request("enqueueTimeline");
    r.file_ids = Some(file_ids);
    r.timeline_mode = Some(mode.into());
    r
}

/// The engine resumes only paused timeline jobs; a failed or cancelled one is
/// retried as a fresh job over the same files, as on macOS.
pub(super) fn job_request(action: &str, job: &CatalogJob) -> CatalogRequest {
    if action == "retryTimeline" {
        let mut ids = job.file_ids.clone();
        ids.sort_unstable();
        ids.dedup();
        let mut r = catalog_request("enqueueTimeline");
        r.file_ids = Some(ids);
        return r;
    }
    let mut r = catalog_request(action);
    r.job_id = Some(job.id.clone());
    r
}

pub(super) fn job_controls(job: &CatalogJob) -> Vec<(&'static str, &'static str)> {
    let mut controls = vec![];
    if !matches!(job.kind.as_str(), "timelineSample" | "catalogIndex") {
        return controls;
    }
    if matches!(job.state.as_str(), "running" | "queued") {
        controls.push(("pauseJob", "Pause"));
    }
    match (job.kind.as_str(), job.state.as_str()) {
        (_, "paused") => controls.push(("resumeJob", "Resume")),
        ("catalogIndex", "failed" | "cancelled") => controls.push(("resumeJob", "Retry")),
        ("timelineSample", "failed" | "cancelled") => controls.push(("retryTimeline", "Retry")),
        _ => {}
    }
    if matches!(job.state.as_str(), "running" | "queued" | "paused") {
        controls.push(("cancelJob", "Cancel"));
    }
    controls
}

// Parent may enable only after accepting the shared Tools Undo atomic no-clobber/cross-volume fix.
pub(super) const LINUX_TOOL_UNDO_SAFETY_ACCEPTED: bool = true;
pub(super) const TOOL_UNDO_UNAVAILABLE_REASON: &str = "Export Undo unavailable: the Linux recovery runtime has not passed atomic no-clobber and cross-volume safety acceptance. Export receipts remain visible; no outputs are moved by this disabled action.";

pub fn tools(engine: Engine) -> gtk::Widget {
    super::file_tools::tools(engine)
}

pub fn chat(engine: Engine) -> gtk::Widget {
    super::chat::chat(engine)
}

pub fn chat_inspector(engine: Engine, on_close: Rc<dyn Fn()>) -> gtk::Widget {
    super::chat::inspector(engine, on_close)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn camera_and_phone_media_play_in_place() {
        for path in ["/v/clip.MTS", "/v/a.m2ts", "/v/b.3gp", "/v/c.mpg", "/a/d.opus", "/a/e.aiff", "/v/f.mp4"] {
            assert!(is_playable_media(path), "{path}");
        }
        for path in ["/p/photo.jpg", "/d/notes.pdf", "/x/no_extension"] {
            assert!(!is_playable_media(path), "{path}");
        }
    }

    fn job(kind: &str, state: &str) -> CatalogJob {
        CatalogJob {
            id: "job".into(),
            kind: kind.into(),
            file_ids: vec![42, 7, 42],
            state: state.into(),
            progress: 0.2,
            error: None,
            created_at: 0.0,
            updated_at: 0.0,
        }
    }
    fn actions(job: &CatalogJob) -> Vec<&'static str> {
        job_controls(job)
            .into_iter()
            .map(|(action, _)| action)
            .collect()
    }
    #[test]
    fn job_controls_follow_engine_transitions() {
        assert_eq!(
            actions(&job("timelineSample", "running")),
            ["pauseJob", "cancelJob"]
        );
        assert_eq!(
            actions(&job("timelineSample", "paused")),
            ["resumeJob", "cancelJob"]
        );
        assert_eq!(actions(&job("timelineSample", "failed")), ["retryTimeline"]);
        assert!(actions(&job("timelineSample", "completed")).is_empty());
        assert_eq!(actions(&job("catalogIndex", "failed")), ["resumeJob"]);
        assert_eq!(
            actions(&job("catalogIndex", "paused")),
            ["resumeJob", "cancelJob"]
        );
        assert!(actions(&job("unknown", "paused")).is_empty());
    }
    #[test]
    fn timeline_requests_carry_files_mode_and_retry_identity() {
        let request = timeline_request(vec![42], "moments");
        assert_eq!(request.action, "enqueueTimeline");
        assert_eq!(request.file_ids.as_deref(), Some(&[42][..]));
        assert_eq!(request.timeline_mode.as_deref(), Some("moments"));
        let retry = job_request("retryTimeline", &job("timelineSample", "failed"));
        assert_eq!(
            (
                retry.action.as_str(),
                retry.file_ids.as_deref(),
                retry.job_id.as_deref()
            ),
            ("enqueueTimeline", Some(&[7, 42][..]), None)
        );
        let pause = job_request("pauseJob", &job("timelineSample", "running"));
        assert_eq!(
            (pause.action.as_str(), pause.job_id.as_deref()),
            ("pauseJob", Some("job"))
        );
    }
}

#[cfg(test)]
#[path = "workbench_regression_tests.rs"]
mod regression_tests;
#[cfg(test)]
pub(super) fn run_review_regressions() {
    regression_tests::run();
}
