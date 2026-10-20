// What the shell knows about running work, and everything derived from it:
// the scan card (macOS `PipelineBlock`), the title-bar activity button and
// popover, sidebar badges and page meta. Pure, so it is unit-tested headless.

use fileid_engine::ipc::{DeepAnalyzeProgress, ScanPhase, ScanProgress};

use super::grouped;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Gold,
    Ai,
    Info,
    Done,
    Danger,
    Warning,
    Paused,
    Muted,
    Text2,
}

impl Tone {
    pub const ALL: [Tone; 9] = [
        Tone::Gold,
        Tone::Ai,
        Tone::Info,
        Tone::Done,
        Tone::Danger,
        Tone::Warning,
        Tone::Paused,
        Tone::Muted,
        Tone::Text2,
    ];

    pub fn css(self) -> &'static str {
        match self {
            Tone::Gold => "tone-gold",
            Tone::Ai => "tone-ai",
            Tone::Info => "tone-info",
            Tone::Done => "tone-done",
            Tone::Danger => "tone-danger",
            Tone::Warning => "tone-warning",
            Tone::Paused => "tone-paused",
            Tone::Muted => "tone-muted",
            Tone::Text2 => "tone-text2",
        }
    }
}

pub const STAGES: [&str; 5] = ["Scan", "Tag", "Faces", "Captions", "Done"];

#[derive(Debug, Default, Clone)]
pub struct Status {
    pub engine_ready: bool,
    pub folder: Option<String>,
    pub progress: Option<ScanProgress>,
    pub phase: Option<ScanPhase>,
    pub paused: bool,
    pub start_requested: bool,
    pub deep: Option<DeepAnalyzeProgress>,
    pub deep_running: bool,
    pub deep_done: Option<u64>,
    pub people: Option<u32>,
    pub restructure_plan: bool,
    pub total_files: u64,
}

#[derive(Debug, PartialEq)]
pub struct Pipeline {
    pub title: String,
    pub fraction: f64,
    pub bar: Tone,
    pub accent: Tone,
    pub sub: String,
    pub stage: usize,
    pub stage_tone: Tone,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ScanControls {
    PickFolder,
    Scan,
    Rescan,
    Running { paused: bool },
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum JobAction {
    PauseScan,
    ResumeScan,
    CancelDeep,
}

#[derive(Debug, PartialEq)]
pub struct Job {
    pub name: &'static str,
    pub sub: String,
    pub state: String,
    pub tone: Tone,
    pub fraction: f64,
    pub action: Option<JobAction>,
    pub open: &'static str,
}

#[derive(Debug, PartialEq)]
pub struct Activity {
    pub label: &'static str,
    pub tone: Tone,
    pub fraction: f64,
    pub idle: bool,
}

fn fraction(done: u64, total: u64) -> f64 {
    if total > 0 {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

pub fn percent(fraction: f64) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0) as u32)
}

impl Status {
    pub fn scan_running(&self) -> bool {
        matches!(
            self.phase,
            Some(ScanPhase::Discovering | ScanPhase::Tagging | ScanPhase::PostScan)
        )
    }

    fn scan_fraction(&self) -> f64 {
        self.progress
            .as_ref()
            .map_or(0.0, |p| fraction(p.processed, p.total))
    }

    fn scan_of_total(p: &ScanProgress) -> String {
        format!(
            "{} of {} files",
            grouped(p.processed),
            grouped(p.total.max(p.discovered))
        )
    }

    pub fn stage(&self) -> usize {
        match self.phase {
            Some(ScanPhase::Discovering) => return 0,
            Some(ScanPhase::Tagging) => return 1,
            Some(ScanPhase::PostScan) => return 2,
            _ => {}
        }
        if self.deep_running {
            3
        } else if self.deep_done.unwrap_or(0) > 0 {
            4
        } else if self.total_files > 0 || self.phase == Some(ScanPhase::Completed) {
            3
        } else {
            0
        }
    }

    pub fn pipeline(&self) -> Pipeline {
        let stage = self.stage();
        let (title, fraction, bar, accent, sub) = self.pipeline_state();
        let stage_tone = if stage == 3 && self.deep_running {
            Tone::Ai
        } else {
            accent
        };
        Pipeline {
            title,
            fraction,
            bar,
            accent,
            sub,
            stage,
            stage_tone,
        }
    }

    fn pipeline_state(&self) -> (String, f64, Tone, Tone, String) {
        let folder = self.folder.as_deref().unwrap_or("This folder");
        if let (Some(p), true) = (self.progress.as_ref(), self.scan_running()) {
            let frac = self.scan_fraction();
            if self.paused {
                return (
                    "Scan paused".into(),
                    frac,
                    Tone::Paused,
                    Tone::Muted,
                    Self::scan_of_total(p),
                );
            }
            let discovering = self.phase == Some(ScanPhase::Discovering);
            let title = match self.phase {
                Some(ScanPhase::Discovering) => "Counting files",
                Some(ScanPhase::PostScan) => "Grouping faces",
                _ => "Scanning",
            };
            let sub = if discovering {
                format!("{} found", grouped(p.discovered))
            } else if p.files_per_second > 0.0 {
                format!("{} · {:.0}/s", Self::scan_of_total(p), p.files_per_second)
            } else {
                Self::scan_of_total(p)
            };
            return (
                title.into(),
                if discovering { 0.02 } else { frac },
                Tone::Gold,
                Tone::Gold,
                sub,
            );
        }
        if self.scan_running() {
            return (
                "Starting".into(),
                0.0,
                Tone::Gold,
                Tone::Gold,
                format!("Reading {folder}"),
            );
        }
        if let (true, Some(d)) = (self.deep_running, self.deep.as_ref()) {
            let sub = format!("{} of {} captioned", grouped(d.processed), grouped(d.total));
            return (
                "Writing captions".into(),
                fraction(d.processed, d.total),
                Tone::Ai,
                Tone::Ai,
                sub,
            );
        }
        if let Some(done) = self.deep_done.filter(|n| *n > 0) {
            return (
                "Everything analyzed".into(),
                1.0,
                Tone::Done,
                Tone::Done,
                format!("{} files captioned", grouped(done)),
            );
        }
        match (self.phase, self.progress.as_ref()) {
            (Some(ScanPhase::Cancelled), p) => {
                let sub = p.map_or_else(String::new, |p| {
                    format!("{} of {} tagged", grouped(p.processed), grouped(p.total))
                });
                return (
                    "Scan cancelled".into(),
                    self.scan_fraction(),
                    Tone::Paused,
                    Tone::Muted,
                    sub,
                );
            }
            (Some(ScanPhase::Failed), _) => {
                return (
                    "Scan failed".into(),
                    0.0,
                    Tone::Paused,
                    Tone::Danger,
                    "Check the engine log".into(),
                );
            }
            _ => {}
        }
        if self.total_files > 0 || self.phase == Some(ScanPhase::Completed) {
            let sub = format!("{} files · captions not started", grouped(self.total_files));
            return ("Scan complete".into(), 1.0, Tone::Gold, Tone::Gold, sub);
        }
        let title = if self.start_requested {
            "Starting"
        } else {
            "Not scanned yet"
        };
        (
            title.into(),
            0.0,
            Tone::Gold,
            Tone::Text2,
            format!("{folder} is ready to scan"),
        )
    }

    pub fn controls(&self) -> ScanControls {
        if self.scan_running() {
            ScanControls::Running {
                paused: self.paused,
            }
        } else if self.folder.is_none() {
            ScanControls::PickFolder
        } else if self.total_files > 0 || self.progress.is_some() {
            ScanControls::Rescan
        } else {
            ScanControls::Scan
        }
    }

    pub fn can_start(&self) -> bool {
        self.engine_ready && !self.start_requested && !self.deep_running && !self.scan_running()
    }

    pub fn activity(&self) -> Activity {
        if self.scan_running() {
            return Activity {
                label: "Scanning",
                tone: Tone::Gold,
                fraction: self.scan_fraction(),
                idle: false,
            };
        }
        if let (true, Some(d)) = (self.deep_running, self.deep.as_ref()) {
            return Activity {
                label: "Captions",
                tone: Tone::Ai,
                fraction: fraction(d.processed, d.total),
                idle: false,
            };
        }
        Activity {
            label: "Idle",
            tone: Tone::Info,
            fraction: 1.0,
            idle: true,
        }
    }

    pub fn jobs(&self) -> Vec<Job> {
        let mut jobs = Vec::new();
        if let Some(p) = self
            .progress
            .as_ref()
            .filter(|_| self.phase.is_some_and(|ph| ph != ScanPhase::Idle))
        {
            let running = self.scan_running();
            let completed = self.phase == Some(ScanPhase::Completed);
            let frac = if completed { 1.0 } else { self.scan_fraction() };
            let state = if completed {
                "DONE".to_owned()
            } else if running {
                format!(
                    "{}{}",
                    if self.paused { "PAUSED " } else { "" },
                    percent(frac)
                )
            } else {
                format!("{:?}", self.phase.unwrap_or(ScanPhase::Idle)).to_uppercase()
            };
            let tone = if completed {
                Tone::Done
            } else if self.paused || !running {
                Tone::Paused
            } else {
                Tone::Gold
            };
            jobs.push(Job {
                name: "Library scan",
                sub: if running {
                    Self::scan_of_total(p)
                } else {
                    format!("{} files", grouped(p.processed))
                },
                state,
                tone,
                fraction: frac,
                action: running.then_some(if self.paused {
                    JobAction::ResumeScan
                } else {
                    JobAction::PauseScan
                }),
                open: "library",
            });
        }
        if let (true, Some(d)) = (self.deep_running, self.deep.as_ref()) {
            let frac = fraction(d.processed, d.total);
            jobs.push(Job {
                name: "Deep Analyze",
                sub: format!(
                    "{} of {} · {}",
                    grouped(d.processed),
                    grouped(d.total),
                    d.model_kind
                ),
                state: percent(frac),
                tone: Tone::Ai,
                fraction: frac,
                action: Some(JobAction::CancelDeep),
                open: "deep-analyze",
            });
        } else if let Some(done) = self.deep_done.filter(|n| *n > 0) {
            jobs.push(Job {
                name: "Deep Analyze",
                sub: format!("{} files", grouped(done)),
                state: "DONE".into(),
                tone: Tone::Done,
                fraction: 1.0,
                action: None,
                open: "deep-analyze",
            });
        }
        jobs
    }

    /// Short trailing sidebar badge; `None` when there is nothing worth saying.
    pub fn badge(&self, page: &str) -> Option<(String, Tone)> {
        match page {
            "library" => {
                if self.scan_running() {
                    let total = self
                        .progress
                        .as_ref()
                        .map_or(0, |p| p.total.max(p.discovered));
                    let text = if total > 0 {
                        percent(self.scan_fraction())
                    } else {
                        "…".into()
                    };
                    return Some((text, Tone::Gold));
                }
                (self.total_files > 0).then(|| (super::compact(self.total_files), Tone::Muted))
            }
            "people" => self
                .people
                .filter(|n| *n > 0)
                .map(|n| (super::compact(u64::from(n)), Tone::Muted)),
            "deep-analyze" => match (self.deep_running, self.deep.as_ref()) {
                (true, Some(d)) if d.total > 0 => {
                    Some((percent(fraction(d.processed, d.total)), Tone::Ai))
                }
                _ => None,
            },
            "restructure" => self
                .restructure_plan
                .then(|| ("Plan".to_owned(), Tone::Gold)),
            _ => None,
        }
    }

    pub fn page_meta(&self, page: &str) -> String {
        match page {
            "library" if self.total_files > 0 => format!("{} files", grouped(self.total_files)),
            "deep-analyze" => match (self.deep_running, self.deep.as_ref()) {
                (true, Some(d)) => format!("{} / {}", grouped(d.processed), grouped(d.total)),
                _ => String::new(),
            },
            "restructure" => self.folder.clone().unwrap_or_default(),
            "settings" => "Linux".into(),
            _ => String::new(),
        }
    }

    pub fn stats(&self) -> Vec<(&'static str, String, Option<String>, Tone)> {
        let Some(p) = self
            .progress
            .as_ref()
            .filter(|_| self.phase.is_some_and(|ph| ph != ScanPhase::Idle))
        else {
            return Vec::new();
        };
        let eta = p
            .eta_seconds
            .filter(|s| *s > 0.0 && self.scan_running())
            .map(|s| format!("{} left", format_eta(s)));
        let rate = (p.files_per_second > 0.0).then(|| format!("{:.1}/s", p.files_per_second));
        let memory_tone = if p.resident_mb > 1200 {
            Tone::Warning
        } else {
            Tone::Text2
        };
        let mut rows = vec![
            ("Found", grouped(p.discovered), eta, Tone::Text2),
            ("Tagged", grouped(p.processed), rate, Tone::Text2),
            (
                "Memory",
                format!("{} MB", p.resident_mb),
                Some(format!("{} MB free", p.available_mb)),
                memory_tone,
            ),
        ];
        if p.failed > 0 {
            rows.push(("Failed", grouped(p.failed), None, Tone::Danger));
        }
        rows
    }
}

pub fn format_eta(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if s >= 3600 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(phase: ScanPhase, processed: u64, total: u64) -> ScanProgress {
        ScanProgress {
            session_id: "s".into(),
            phase,
            total,
            discovered: total,
            processed,
            failed: 0,
            files_per_second: 142.4,
            eta_seconds: Some(125.0),
            resident_mb: 640,
            available_mb: 8000,
        }
    }

    fn scanning(processed: u64, total: u64) -> Status {
        Status {
            engine_ready: true,
            folder: Some("Photos".into()),
            progress: Some(progress(ScanPhase::Tagging, processed, total)),
            phase: Some(ScanPhase::Tagging),
            ..Status::default()
        }
    }

    #[test]
    fn fresh_folder_is_ready_to_scan_at_step_one() {
        let status = Status {
            folder: Some("Photos".into()),
            engine_ready: true,
            ..Status::default()
        };
        let p = status.pipeline();
        assert_eq!(p.title, "Not scanned yet");
        assert_eq!(p.sub, "Photos is ready to scan");
        assert_eq!(p.stage, 0);
        assert_eq!(status.controls(), ScanControls::Scan);
        assert!(status.can_start());
        assert_eq!(Status::default().controls(), ScanControls::PickFolder);
    }

    #[test]
    fn tagging_reports_progress_rate_and_step_two() {
        let status = scanning(250, 1000);
        let p = status.pipeline();
        assert_eq!(p.title, "Scanning");
        assert_eq!(p.sub, "250 of 1,000 files · 142/s");
        assert!((p.fraction - 0.25).abs() < 1e-9);
        assert_eq!((p.stage, p.bar), (1, Tone::Gold));
        assert_eq!(status.controls(), ScanControls::Running { paused: false });
        assert_eq!(status.badge("library"), Some(("25%".into(), Tone::Gold)));
        assert_eq!(
            status.activity(),
            Activity {
                label: "Scanning",
                tone: Tone::Gold,
                fraction: 0.25,
                idle: false
            }
        );
        assert!(!status.can_start());
    }

    #[test]
    fn paused_scan_greys_the_bar_and_offers_resume() {
        let status = Status {
            paused: true,
            ..scanning(10, 40)
        };
        let p = status.pipeline();
        assert_eq!(
            (p.title.as_str(), p.bar, p.accent),
            ("Scan paused", Tone::Paused, Tone::Muted)
        );
        let job = &status.jobs()[0];
        assert_eq!(job.state, "PAUSED 25%");
        assert_eq!(job.action, Some(JobAction::ResumeScan));
    }

    #[test]
    fn completed_library_waits_for_captions_then_finishes() {
        let mut status = Status {
            folder: Some("Photos".into()),
            total_files: 12_400,
            phase: Some(ScanPhase::Completed),
            progress: Some(progress(ScanPhase::Completed, 12_400, 12_400)),
            ..Status::default()
        };
        let p = status.pipeline();
        assert_eq!((p.title.as_str(), p.stage), ("Scan complete", 3));
        assert_eq!(p.sub, "12,400 files · captions not started");
        assert_eq!(status.controls(), ScanControls::Rescan);
        assert_eq!(status.badge("library"), Some(("12k".into(), Tone::Muted)));
        assert_eq!(status.jobs()[0].state, "DONE");

        status.deep_running = true;
        status.deep = Some(DeepAnalyzeProgress {
            processed: 30,
            total: 120,
            eta_seconds: None,
            current_path: None,
            model_kind: "qwen".into(),
            current_caption: None,
        });
        let p = status.pipeline();
        assert_eq!(
            (p.title.as_str(), p.bar, p.stage_tone),
            ("Writing captions", Tone::Ai, Tone::Ai)
        );
        assert_eq!(status.badge("deep-analyze"), Some(("25%".into(), Tone::Ai)));
        assert_eq!(status.page_meta("deep-analyze"), "30 / 120");
        assert_eq!(status.activity().label, "Captions");
        assert_eq!(status.jobs()[1].action, Some(JobAction::CancelDeep));

        status.deep_running = false;
        status.deep_done = Some(120);
        let p = status.pipeline();
        assert_eq!(
            (p.title.as_str(), p.stage, p.bar),
            ("Everything analyzed", 4, Tone::Done)
        );
        assert!(status.activity().idle);
    }

    #[test]
    fn cancelled_and_failed_scans_are_not_reported_as_success() {
        let cancelled = Status {
            folder: Some("Photos".into()),
            phase: Some(ScanPhase::Cancelled),
            progress: Some(progress(ScanPhase::Cancelled, 5, 20)),
            ..Status::default()
        };
        assert_eq!(cancelled.pipeline().title, "Scan cancelled");
        assert_eq!(cancelled.jobs()[0].state, "CANCELLED");
        let failed = Status {
            phase: Some(ScanPhase::Failed),
            ..cancelled
        };
        let p = failed.pipeline();
        assert_eq!((p.title.as_str(), p.accent), ("Scan failed", Tone::Danger));
    }

    #[test]
    fn badges_stay_quiet_until_they_say_something() {
        let status = Status::default();
        for page in ["library", "people", "deep-analyze", "restructure", "chat"] {
            assert_eq!(status.badge(page), None, "{page}");
        }
        let status = Status {
            people: Some(14),
            restructure_plan: true,
            ..Status::default()
        };
        assert_eq!(status.badge("people"), Some(("14".into(), Tone::Muted)));
        assert_eq!(
            status.badge("restructure"),
            Some(("Plan".into(), Tone::Gold))
        );
    }

    #[test]
    fn scan_details_list_counters_and_flag_failures() {
        let mut status = scanning(100, 400);
        status.progress.as_mut().unwrap().failed = 3;
        let rows = status.stats();
        assert_eq!(
            rows[0],
            ("Found", "400".into(), Some("2m left".into()), Tone::Text2)
        );
        assert_eq!(rows[1].2.as_deref(), Some("142.4/s"));
        assert_eq!(rows[3], ("Failed", "3".into(), None, Tone::Danger));
        assert!(Status::default().stats().is_empty());
        assert_eq!(format_eta(3725.0), "1h 2m");
    }
}
