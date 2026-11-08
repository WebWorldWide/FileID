// Related-take proposals from the library-wide takeGrouping job, kept free of
// GTK: the relation chip, plain-language evidence and the job requests.
// Lockstep with macOS TakeProposals and WinUI TakeProposalPolicy.

use fileid_engine::ipc::{CatalogHit, CatalogJob, CatalogRequest, CatalogResponse, CatalogTakeAutoGroup};

use super::takes_model as model;
use super::workbench_state::catalog_request;
use crate::shell::status::Tone;

pub const JOB_KIND: &str = "takeGrouping";
pub const JOB_ID: &str = "take-grouping";

pub fn relation_label(relation: Option<&str>) -> Option<&'static str> {
    match relation? {
        "repeatedAttempts" => Some("Repeated tries"),
        "alternateViews" => Some("Other angles"),
        "uncertain" => Some("Not sure"),
        _ => None,
    }
}

pub fn relation_tone(relation: Option<&str>) -> Tone {
    match relation {
        Some("repeatedAttempts") => Tone::Gold,
        Some("alternateViews") => Tone::Info,
        _ => Tone::Ai,
    }
}

pub fn relation_help(relation: Option<&str>) -> Option<&'static str> {
    match relation? {
        "repeatedAttempts" => Some("One camera took these one after another, so they are separate tries of the same shot."),
        "alternateViews" => Some("Different cameras caught the same moment, so these are other angles or copies rather than separate tries."),
        "uncertain" => Some("Camera details are missing or mixed, so FileID can't tell separate tries from other angles."),
        _ => None,
    }
}

pub fn evidence_sentence(code: &str) -> Option<&'static str> {
    Some(match code {
        "captureSession" => "Taken close together.",
        "visualSimilarity" => "They look alike.",
        "sharedSubject" => "The same people are in every try.",
        "sameCamera" => "One camera took them all.",
        "simultaneousCameras" => "Different cameras took them at the same moment.",
        "mixedCameras" => "They come from a mix of cameras.",
        "cameraDetailsMissing" => "Some have no camera details.",
        _ => return None,
    })
}

pub fn evidence_text(codes: Option<&[String]>) -> String {
    codes.unwrap_or_default().iter().filter_map(|code| evidence_sentence(code)).collect::<Vec<_>>().join(" ")
}

/// The review page's intro: the evidence and what the relation means, then how to pick.
pub fn summary(group: &CatalogTakeAutoGroup) -> String {
    const PICK: &str = "FileID starred the one that looks best. Check it, then keep it and clear out the rest.";
    if group.relation.is_none() {
        return format!("Tries of the same shot, taken moments apart. {PICK}");
    }
    let evidence = evidence_text(group.evidence.as_deref());
    let help = relation_help(group.relation.as_deref()).unwrap_or_default();
    [evidence.as_str(), help, PICK].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ")
}

pub fn job(jobs: &[CatalogJob]) -> Option<CatalogJob> {
    jobs.iter().find(|job| job.kind == JOB_KIND).cloned()
}

/// Find related tries is offered unless a run is already queued or running.
pub fn can_find(job: Option<&CatalogJob>) -> bool {
    !matches!(job.map(|job| job.state.as_str()), Some("queued" | "running"))
}

/// True when the job has just finished a run, so the proposals should be read again.
pub fn finished(previous: Option<&CatalogJob>, current: Option<&CatalogJob>) -> bool {
    let Some(current) = current.filter(|job| job.state == "completed") else { return false };
    previous.is_none_or(|previous| previous.state != "completed" || previous.updated_at != current.updated_at)
}

/// The job row shows while a run is pending, paused or stopped; a finished run hides it.
pub fn show_job(job: Option<&CatalogJob>) -> bool {
    job.is_some_and(|job| job.state != "completed")
}

/// A catalog response the engine pushed while the job runs (its request ID is the job's).
pub fn is_job_push(response: &CatalogResponse) -> bool {
    response.request_id == JOB_ID && job(&response.jobs).is_some()
}

/// Pause, Resume, Retry and Cancel for the job row; a stopped run is queued again.
pub fn controls(job: &CatalogJob) -> Vec<(&'static str, &'static str)> {
    let mut controls = vec![];
    if matches!(job.state.as_str(), "running" | "queued") {
        controls.push(("pauseJob", "Pause"));
    }
    match job.state.as_str() {
        "paused" => controls.push(("resumeJob", "Resume")),
        "failed" | "cancelled" => controls.push(("enqueueTakeGrouping", "Retry")),
        _ => {}
    }
    if matches!(job.state.as_str(), "running" | "queued" | "paused") {
        controls.push(("cancelJob", "Cancel"));
    }
    controls
}

pub fn control_request(action: &str, job: &CatalogJob) -> CatalogRequest {
    if action == "enqueueTakeGrouping" {
        return enqueue_request();
    }
    let mut request = catalog_request(action);
    request.job_id = Some(job.id.clone());
    request
}

pub fn enqueue_request() -> CatalogRequest {
    let mut request = catalog_request("enqueueTakeGrouping");
    request.limit = None;
    request
}

pub fn proposals_request() -> CatalogRequest {
    let mut request = catalog_request("takeGroupProposals");
    request.limit = Some(100);
    request
}

pub fn header(proposals: &[CatalogTakeAutoGroup]) -> String {
    format!("RELATED TRIES · {}", model::open_count(proposals))
}

pub fn empty_text(job: Option<&CatalogJob>) -> &'static str {
    if job.is_none() { "Not looked yet. Find related tries to search the library." } else { "No related tries found yet." }
}

pub fn spoken(group: &CatalogTakeAutoGroup, date: &str) -> String {
    let relation = relation_label(group.relation.as_deref()).map(|label| format!(", {label}")).unwrap_or_default();
    let skipped = if model::is_open(group) { "" } else { ", skipped" };
    let evidence = evidence_text(group.evidence.as_deref());
    let tail = if evidence.is_empty() { String::new() } else { format!(". {evidence}") };
    format!("{}{relation}, {date}{skipped}{tail}", model::group_title(group))
}

/// The members to put in a new group when the person saves a proposal: one per try.
pub fn save_hits(group: &CatalogTakeAutoGroup) -> Vec<CatalogHit> {
    let kind = if model::is_video(group) { "video" } else { "image" };
    group
        .shots
        .iter()
        .map(|shot| CatalogHit {
            file_id: shot.primary_file_id,
            path: shot.path.clone(),
            kind: kind.into(),
            text: String::new(),
            evidence_id: None,
            start_seconds: None,
            page: None,
            selection_match: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::CatalogTakeShot;

    fn job_in(state: &str, updated: f64) -> CatalogJob {
        CatalogJob {
            id: JOB_ID.into(),
            kind: JOB_KIND.into(),
            file_ids: vec![],
            state: state.into(),
            progress: 0.5,
            error: None,
            created_at: 0.0,
            updated_at: updated,
        }
    }

    fn group(key: &str, status: &str, relation: Option<&str>) -> CatalogTakeAutoGroup {
        CatalogTakeAutoGroup {
            group_key: key.into(),
            kind: "photos".into(),
            status: status.into(),
            captured_at: 0.0,
            similarity: 0.9,
            shots: vec![
                CatalogTakeShot { primary_file_id: 4, file_ids: vec![4, 5], path: "/p/a.JPG".into(), captured_at: None },
                CatalogTakeShot { primary_file_id: 6, file_ids: vec![6], path: "/p/b.JPG".into(), captured_at: None },
            ],
            relation: relation.map(str::to_owned),
            evidence: Some(vec!["captureSession".into(), "simultaneousCameras".into()]),
        }
    }

    #[test]
    fn relation_chips_and_evidence_text() {
        assert_eq!(relation_label(Some("repeatedAttempts")), Some("Repeated tries"));
        assert_eq!(relation_label(Some("alternateViews")), Some("Other angles"));
        assert_eq!(relation_label(Some("uncertain")), Some("Not sure"));
        assert_eq!(relation_label(None), None);
        assert_eq!(relation_label(Some("other")), None);
        assert_eq!(relation_tone(Some("repeatedAttempts")), Tone::Gold);
        assert_eq!(relation_tone(Some("alternateViews")), Tone::Info);
        assert_eq!(relation_tone(Some("uncertain")), Tone::Ai);
        for relation in ["repeatedAttempts", "alternateViews", "uncertain"] {
            assert!(relation_help(Some(relation)).is_some());
        }
        for code in ["captureSession", "visualSimilarity", "sharedSubject", "sameCamera", "simultaneousCameras", "mixedCameras", "cameraDetailsMissing"] {
            assert!(evidence_sentence(code).is_some());
        }
        let codes: Vec<String> = ["captureSession", "visualSimilarity", "future", "sameCamera"].map(String::from).to_vec();
        assert_eq!(evidence_text(Some(&codes)), "Taken close together. They look alike. One camera took them all.");
        assert_eq!(evidence_text(None), "");
        let mut plain = group("k", "open", None);
        plain.evidence = None;
        assert!(summary(&plain).starts_with("Tries of the same shot"));
        assert!(summary(&group("k", "open", Some("alternateViews")))
            .starts_with("Taken close together. Different cameras took them at the same moment. Different cameras caught"));
    }

    #[test]
    fn job_controls_pushes_and_reload() {
        let timeline = CatalogJob { kind: "timelineSample".into(), id: "t".into(), ..job_in("running", 0.0) };
        assert_eq!(job(&[timeline.clone(), job_in("paused", 1.0)]).map(|j| j.state), Some("paused".to_owned()));
        assert!(can_find(None) && can_find(Some(&job_in("paused", 1.0))) && can_find(Some(&job_in("completed", 1.0))));
        assert!(!can_find(Some(&job_in("queued", 1.0))) && !can_find(Some(&job_in("running", 1.0))));
        assert!(finished(Some(&job_in("running", 1.0)), Some(&job_in("completed", 1.0))));
        assert!(finished(None, Some(&job_in("completed", 1.0))));
        assert!(finished(Some(&job_in("completed", 1.0)), Some(&job_in("completed", 2.0))));
        assert!(!finished(Some(&job_in("completed", 1.0)), Some(&job_in("completed", 1.0))));
        assert!(!finished(Some(&job_in("queued", 1.0)), Some(&job_in("running", 1.0))));
        assert!(!show_job(None) && !show_job(Some(&job_in("completed", 1.0))) && show_job(Some(&job_in("paused", 1.0))));
        let names = |state: &str| controls(&job_in(state, 1.0)).into_iter().map(|(_, name)| name).collect::<Vec<_>>();
        assert_eq!(names("running"), ["Pause", "Cancel"]);
        assert_eq!(names("paused"), ["Resume", "Cancel"]);
        assert_eq!(names("failed"), ["Retry"]);
        assert!(names("completed").is_empty());
        assert_eq!(control_request("enqueueTakeGrouping", &job_in("failed", 1.0)).action, "enqueueTakeGrouping");
        let resume = control_request("resumeJob", &job_in("paused", 1.0));
        assert_eq!((resume.action.as_str(), resume.job_id.as_deref()), ("resumeJob", Some(JOB_ID)));
        let read = proposals_request();
        assert_eq!((read.action.as_str(), read.limit), ("takeGroupProposals", Some(100)));
        assert_eq!(enqueue_request().action, "enqueueTakeGrouping");
        assert_eq!(empty_text(None), "Not looked yet. Find related tries to search the library.");
    }

    #[test]
    fn labels_and_saved_members() {
        assert_eq!(
            spoken(&group("k", "skipped", Some("alternateViews")), "Sat 4 Jul"),
            "2 photos, Other angles, Sat 4 Jul, skipped. Taken close together. Different cameras took them at the same moment."
        );
        assert_eq!(header(&[group("a", "open", None), group("b", "skipped", None)]), "RELATED TRIES · 1");
        let hits = save_hits(&group("k", "open", None));
        assert_eq!(hits.iter().map(|h| h.file_id).collect::<Vec<_>>(), [4, 6]);
        assert!(hits.iter().all(|h| h.kind == "image"));
    }
}
