// What a page built after startup has missed. Pages learn about running work
// (a scan, Describe Files, a model download, a speed test) only from engine
// events, so a page first opened in the middle of a job would look idle. This
// keeps the newest event of each job that is still running, and a page built
// late gets them first, followed by a `Ready` when the engine is already up
// (pages load their data on `Ready`). Replies to a page's own requests are
// never kept: those belong to the page that asked.

use std::rc::Rc;

use crate::engine_client::EngineState;
use fileid_engine::ipc::ScanPhase;

#[derive(Default)]
pub struct JobReplay {
    scanning: Option<Rc<EngineState>>,
    scan_progress: Option<Rc<EngineState>>,
    deep_starting: Option<Rc<EngineState>>,
    deep_progress: Option<Rc<EngineState>>,
    deep_file: Option<Rc<EngineState>>,
    downloads: Vec<(String, Rc<EngineState>)>,
    benchmark: Option<Rc<EngineState>>,
}

impl JobReplay {
    pub fn observe(&mut self, event: &Rc<EngineState>) {
        match &**event {
            EngineState::Scanning => {
                self.scanning = Some(event.clone());
                self.scan_progress = None;
            }
            EngineState::Progress(progress) => {
                if is_scan_end(progress.phase) {
                    self.end_scan();
                } else {
                    self.scan_progress = Some(event.clone());
                }
            }
            EngineState::ScanPhase(phase) if is_scan_end(*phase) => self.end_scan(),
            EngineState::ScanComplete(_) => self.end_scan(),
            EngineState::DeepAnalyzeStarting(_) => {
                self.end_deep();
                self.deep_starting = Some(event.clone());
            }
            EngineState::DeepAnalyzeProgress(_) => {
                self.deep_starting = None;
                self.deep_progress = Some(event.clone());
            }
            EngineState::DeepAnalyzeFileDone(_) => self.deep_file = Some(event.clone()),
            EngineState::DeepAnalyzeComplete(_) => self.end_deep(),
            EngineState::ModelDownloadProgress(progress) => {
                self.downloads.retain(|(kind, _)| *kind != progress.model_kind);
                if progress.fraction.is_finite() && progress.fraction < 1.0 {
                    self.downloads.push((progress.model_kind.clone(), event.clone()));
                }
            }
            EngineState::BenchmarkProgress(_) => self.benchmark = Some(event.clone()),
            EngineState::BenchmarkResult(_) => self.benchmark = None,
            EngineState::Error { kind, model_kind, .. } => {
                if let Some(model_kind) = model_kind {
                    self.downloads.retain(|(kind, _)| kind != model_kind);
                }
                if kind.starts_with("deep_analyze") && crate::tabs::deep_analyze::error_ends_deep_run(kind) {
                    self.end_deep();
                }
                if kind == "models_not_installed" || kind == "scan_failed" {
                    self.end_scan();
                }
            }
            EngineState::Failed(_) | EngineState::Exited => *self = Self::default(),
            _ => {}
        }
    }

    /// Delivered to a page built now, in the order a page built at launch
    /// would have seen them.
    pub fn snapshot(&self, engine_ready: bool) -> Vec<Rc<EngineState>> {
        let mut events = Vec::new();
        if engine_ready {
            events.push(Rc::new(EngineState::Ready));
        }
        events.extend(self.scanning.iter().cloned());
        events.extend(self.scan_progress.iter().cloned());
        events.extend(self.deep_starting.iter().cloned());
        events.extend(self.deep_progress.iter().cloned());
        events.extend(self.deep_file.iter().cloned());
        events.extend(self.downloads.iter().map(|(_, event)| event.clone()));
        events.extend(self.benchmark.iter().cloned());
        events
    }

    fn end_scan(&mut self) {
        self.scanning = None;
        self.scan_progress = None;
    }

    fn end_deep(&mut self) {
        self.deep_starting = None;
        self.deep_progress = None;
        self.deep_file = None;
    }
}

fn is_scan_end(phase: ScanPhase) -> bool {
    matches!(
        phase,
        ScanPhase::Idle | ScanPhase::Completed | ScanPhase::Cancelled | ScanPhase::Failed
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::{
        BenchmarkProgress, DeepAnalyzeComplete, DeepAnalyzeProgress, DeepAnalyzeStarting, DeepAnalyzeStartingPhase,
        ModelDownloadProgress, ScanProgress,
    };

    fn event(state: EngineState) -> Rc<EngineState> {
        Rc::new(state)
    }

    fn download(kind: &str, fraction: f64) -> Rc<EngineState> {
        event(EngineState::ModelDownloadProgress(ModelDownloadProgress {
            model_kind: kind.into(),
            fraction,
            message: String::new(),
            bytes_done: None,
            total_bytes: None,
        }))
    }

    fn scan_progress(phase: ScanPhase, processed: u64) -> Rc<EngineState> {
        event(EngineState::Progress(ScanProgress {
            session_id: "s".into(),
            phase,
            total: 100,
            discovered: 100,
            processed,
            failed: 0,
            files_per_second: 1.0,
            eta_seconds: None,
            resident_mb: 0,
            available_mb: 0,
        }))
    }

    fn deep_progress(processed: u64) -> Rc<EngineState> {
        event(EngineState::DeepAnalyzeProgress(DeepAnalyzeProgress {
            processed,
            total: 10,
            eta_seconds: None,
            current_path: None,
            model_kind: "m".into(),
            current_caption: None,
        }))
    }

    fn deep_done(cancelled: bool) -> Rc<EngineState> {
        event(EngineState::DeepAnalyzeComplete(DeepAnalyzeComplete {
            processed: 10,
            failed: 0,
            total_seconds: 1.0,
            model_kind: "m".into(),
            cancelled,
        }))
    }

    fn names(events: &[Rc<EngineState>]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match &**event {
                EngineState::Ready => "ready",
                EngineState::Scanning => "scanning",
                EngineState::Progress(_) => "progress",
                EngineState::DeepAnalyzeStarting(_) => "deep-starting",
                EngineState::DeepAnalyzeProgress(_) => "deep-progress",
                EngineState::ModelDownloadProgress(_) => "download",
                EngineState::BenchmarkProgress(_) => "benchmark",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn an_idle_engine_replays_only_ready_and_only_when_it_is_up() {
        let replay = JobReplay::default();
        assert!(replay.snapshot(false).is_empty());
        assert_eq!(names(&replay.snapshot(true)), ["ready"]);
    }

    #[test]
    fn a_running_scan_replays_its_start_and_newest_progress_until_it_ends() {
        let mut replay = JobReplay::default();
        replay.observe(&event(EngineState::Scanning));
        replay.observe(&scan_progress(ScanPhase::Tagging, 10));
        replay.observe(&scan_progress(ScanPhase::Tagging, 40));
        let seen = replay.snapshot(true);
        assert_eq!(names(&seen), ["ready", "scanning", "progress"]);
        assert!(matches!(&*seen[2], EngineState::Progress(p) if p.processed == 40));
        replay.observe(&event(EngineState::ScanComplete(100)));
        assert_eq!(names(&replay.snapshot(true)), ["ready"]);
    }

    #[test]
    fn a_cancelled_or_failed_scan_is_not_replayed() {
        let mut replay = JobReplay::default();
        replay.observe(&event(EngineState::Scanning));
        replay.observe(&event(EngineState::ScanPhase(ScanPhase::Cancelled)));
        assert!(replay.snapshot(false).is_empty());
        replay.observe(&event(EngineState::Scanning));
        replay.observe(&scan_progress(ScanPhase::Failed, 5));
        assert!(replay.snapshot(false).is_empty());
    }

    #[test]
    fn describe_files_replays_the_newest_stage_and_stops_when_it_completes() {
        let mut replay = JobReplay::default();
        replay.observe(&event(EngineState::DeepAnalyzeStarting(DeepAnalyzeStarting {
            model_kind: "m".into(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: "Loading".into(),
        })));
        assert_eq!(names(&replay.snapshot(false)), ["deep-starting"]);
        replay.observe(&deep_progress(2));
        replay.observe(&deep_progress(7));
        let seen = replay.snapshot(false);
        assert_eq!(names(&seen), ["deep-progress"]);
        assert!(matches!(&*seen[0], EngineState::DeepAnalyzeProgress(p) if p.processed == 7));
        replay.observe(&deep_done(false));
        assert!(replay.snapshot(false).is_empty());
    }

    #[test]
    fn describe_files_ends_on_a_run_ending_error_but_not_on_a_busy_bounce() {
        let mut replay = JobReplay::default();
        replay.observe(&deep_progress(1));
        let busy = EngineState::Error {
            kind: "deep_analyze_already_running".into(),
            message: String::new(),
            model_kind: None,
        };
        replay.observe(&event(busy));
        assert_eq!(names(&replay.snapshot(false)), ["deep-progress"]);
        let failed = EngineState::Error { kind: "deep_analyze_failed".into(), message: String::new(), model_kind: None };
        replay.observe(&event(failed));
        assert!(replay.snapshot(false).is_empty());
    }

    #[test]
    fn each_model_download_keeps_its_newest_progress_until_it_finishes_or_fails() {
        let mut replay = JobReplay::default();
        replay.observe(&download("clip", 0.2));
        replay.observe(&download("face", 0.5));
        replay.observe(&download("clip", 0.6));
        let seen = replay.snapshot(false);
        assert_eq!(names(&seen), ["download", "download"]);
        let fractions: Vec<f64> = seen
            .iter()
            .filter_map(|event| match &**event {
                EngineState::ModelDownloadProgress(p) => Some(p.fraction),
                _ => None,
            })
            .collect();
        assert_eq!(fractions, [0.5, 0.6]);
        replay.observe(&download("clip", 1.0));
        assert_eq!(names(&replay.snapshot(false)), ["download"]);
        let cancelled = EngineState::Error {
            kind: "prewarm_cancelled".into(),
            message: String::new(),
            model_kind: Some("face".into()),
        };
        replay.observe(&event(cancelled));
        assert!(replay.snapshot(false).is_empty());
    }

    #[test]
    fn a_speed_test_is_replayed_until_its_result() {
        let mut replay = JobReplay::default();
        replay.observe(&event(EngineState::BenchmarkProgress(BenchmarkProgress {
            request_id: "b".into(),
            model_kind: "m".into(),
            phase: "warm".into(),
            completed: 1,
            total: 4,
        })));
        assert_eq!(names(&replay.snapshot(false)), ["benchmark"]);
        replay.observe(&event(EngineState::BenchmarkResult(fileid_engine::ipc::BenchmarkResult {
            request_id: "b".into(),
            model_kind: "m".into(),
            status: "ok".into(),
            message: None,
            benchmark: None,
        })));
        assert!(replay.snapshot(false).is_empty());
    }

    #[test]
    fn a_restarted_engine_forgets_every_running_job() {
        let mut replay = JobReplay::default();
        replay.observe(&event(EngineState::Scanning));
        replay.observe(&deep_progress(3));
        replay.observe(&download("clip", 0.3));
        replay.observe(&event(EngineState::Exited));
        assert!(replay.snapshot(false).is_empty());
    }
}
