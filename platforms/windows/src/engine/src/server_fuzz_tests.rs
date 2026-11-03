//! Randomized IPC fuzzer. Mutates valid command frames (the schema exemplars
//! and every command in the shared conformance cases) and feeds them through
//! `handle_line` against a temporary state directory and catalog. Asserts no
//! panic anywhere on the engine's threads, no hung command, and a structured
//! error for every frame that fails to decode or validate.
//!
//! The normal run does a few thousand lines. Set `FILEID_FUZZ_IPC_ITERATIONS`
//! for a long run and `FILEID_FUZZ_SEED` to replay one.

use super::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
    pub(crate) fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// Commands whose handlers walk an arbitrary path, launch a process or open a
/// network connection. Their frames are decoded and validated, not dispatched.
const DECODE_ONLY: &[&str] = &[
    "startScan",
    "prewarmModel",
    "benchmarkModel",
    "generateVideoThumbnail",
    "verifyCudaPack",
    "requestStatus",
    "deepAnalyzeFile",
    "deepAnalyzeFolder",
    "deepAnalyzeAll",
];

fn expand(value: &mut Value, root: &str) {
    match value {
        Value::String(text) => {
            let mut out = String::new();
            let mut rest = text.as_str();
            while let Some(open) = rest.find('{') {
                let Some(close) = rest[open..].find('}') else { break };
                out.push_str(&rest[..open]);
                let token = &rest[open + 1..open + close];
                match token.split_once(':') {
                    Some((_, rel)) => {
                        out.push_str(root);
                        out.push('/');
                        out.push_str(rel);
                    }
                    None => out.push_str(root),
                }
                rest = &rest[open + close + 1..];
            }
            out.push_str(rest);
            *text = out;
        }
        Value::Object(map) if map.contains_key("$var") => *value = json!("fuzz-var"),
        Value::Object(map) => map.values_mut().for_each(|v| expand(v, root)),
        Value::Array(items) => items.iter_mut().for_each(|v| expand(v, root)),
        _ => {}
    }
}

fn request(kind: &str, step: &Value, extra: Value) -> Value {
    let mut body = json!({"requestID": "fuzz-request"});
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    if let (Some(body), Some(step)) = (body.as_object_mut(), step.as_object()) {
        body.extend(step.clone());
    }
    json!({ kind: {"request": body} })
}

fn seeds(root: &str) -> Vec<Value> {
    let mut out: Vec<Value> = crate::ipc::conformance::command_exemplars()
        .into_iter()
        .map(|payload| serde_json::to_value(payload).expect("exemplar serializes"))
        .collect();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../shared/test-corpus/conformance");
    let mut files: Vec<_> = std::fs::read_dir(&dir).expect("conformance cases").flatten().map(|e| e.path()).collect();
    files.sort();
    for file in files {
        let cases: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        for step in cases.as_array().into_iter().flatten().filter_map(|c| c["steps"].as_array()).flatten() {
            if let Some(command) = step.get("command") {
                out.push(command.clone());
            } else if let Some(tool) = step.get("tool") {
                out.push(request("toolRequest", tool, json!({})));
            } else if let Some(chat) = step.get("chat") {
                out.push(request("chatRequest", chat, json!({"conversationID": "fuzz-conversation"})));
            } else if let Some(catalog) = step.get("catalog") {
                out.push(request("catalogRequest", catalog, json!({})));
            }
        }
    }
    for seed in &mut out {
        expand(seed, root);
    }
    out
}

fn interesting(rng: &mut Rng) -> Value {
    match rng.below(26) {
        0 => Value::Null,
        1 => json!(true),
        2 => json!(0),
        3 => json!(-1),
        4 => json!(i64::MAX),
        5 => json!(i64::MIN),
        6 => json!(u64::MAX),
        7 => json!(1e308),
        8 => json!(-0.5),
        9 => json!(""),
        10 => json!("\u{0}"),
        11 => json!("a\u{0}b/c"),
        12 => json!("../../../../../../etc/passwd"),
        13 => json!(r"\\?\UNC\server\share\x"),
        14 => json!("C:\\Windows\\System32\\x"),
        15 => json!("é𝄞\u{202E}\u{FEFF}名"),
        16 => json!("A".repeat(1 + rng.below(300_000))),
        17 => json!([]),
        18 => json!({}),
        19 => json!([1, -1, 0, i64::MAX]),
        20 => Value::Array((0..rng.below(150_000)).map(|i| json!(i)).collect()),
        21 => Value::Array((0..rng.below(2_000)).map(|i| json!(format!("tag{i}"))).collect()),
        22 => {
            let mut nested = json!(1);
            for _ in 0..(50 + rng.below(200)) {
                nested = json!([nested]);
            }
            nested
        }
        23 => json!(rng.next() as i64),
        24 => json!(Some(f64::from_bits(rng.next())).filter(|v| v.is_finite()).unwrap_or(0.0)),
        _ => json!("/tmp/fileid-fuzz-path"),
    }
}

const KEYS: &[&str] = &[
    "fileID", "fileIDs", "path", "rootPath", "libraryRoot", "moves", "tags", "mode", "requestID", "action", "query",
    "limit", "personID", "faceID", "modelKind", "excludedPaths", "renames", "newName", "batchID", "chapter",
    "destination", "recipe", "operationID", "useModel", "text", "queryVector", "jobID", "planID",
];

fn mutate(value: &mut Value, rng: &mut Rng, tags: &[String], depth: usize) {
    let here = depth > 6 || rng.chance(30);
    match value {
        Value::Object(map) if !map.is_empty() && !here => {
            let key = map.keys().nth(rng.below(map.len())).cloned().unwrap_or_default();
            if let Some(child) = map.get_mut(&key) {
                mutate(child, rng, tags, depth + 1);
            }
        }
        Value::Array(items) if !items.is_empty() && !here => {
            let at = rng.below(items.len());
            mutate(&mut items[at], rng, tags, depth + 1);
        }
        Value::Object(map) => match rng.below(5) {
            0 if !map.is_empty() => {
                let key = map.keys().nth(rng.below(map.len())).cloned().unwrap_or_default();
                map.remove(&key);
            }
            1 if map.len() == 1 && depth == 0 => {
                let (_, inner) = map.iter().next().map(|(k, v)| (k.clone(), v.clone())).unwrap();
                map.clear();
                map.insert(tags[rng.below(tags.len())].clone(), inner);
            }
            2 => {
                map.insert(KEYS[rng.below(KEYS.len())].to_string(), interesting(rng));
            }
            _ => *value = interesting(rng),
        },
        Value::Array(items) => match rng.below(4) {
            0 if !items.is_empty() => {
                let copy = items[rng.below(items.len())].clone();
                let cap = if rng.chance(10) { 50_000 } else { 8 };
                let times = 1 + rng.below(cap);
                items.extend(std::iter::repeat_n(copy, times));
            }
            1 => items.truncate(rng.below(items.len() + 1)),
            2 => items.push(interesting(rng)),
            _ => *value = interesting(rng),
        },
        _ => *value = interesting(rng),
    }
}

/// Keeps every value's JSON type so the frame still decodes and reaches the
/// handler with hostile contents.
fn mutate_typed(value: &mut Value, rng: &mut Rng) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            let key = map.keys().nth(rng.below(map.len())).cloned().unwrap_or_default();
            if let Some(child) = map.get_mut(&key) {
                mutate_typed(child, rng);
            }
        }
        Value::Array(items) if !items.is_empty() && rng.chance(70) => {
            let at = rng.below(items.len());
            mutate_typed(&mut items[at], rng);
        }
        Value::Array(items) if !items.is_empty() => {
            let copy = items[rng.below(items.len())].clone();
            let cap = if rng.chance(10) { 120_000 } else { 16 };
            let times = rng.below(cap);
            items.extend(std::iter::repeat_n(copy, times));
        }
        Value::String(text) => {
            *text = match rng.below(12) {
                0 => String::new(),
                1 => format!("{text}\u{0}"),
                2 => "../".repeat(1 + rng.below(40)) + "etc/passwd",
                3 => r"\\?\C:\".to_string() + &"a\\".repeat(rng.below(5000)),
                4 => "é𝄞\u{202E}名".repeat(1 + rng.below(3000)),
                5 => "A".repeat(rng.below(200_000)),
                6 => text.chars().rev().collect(),
                7 => format!("{text}{text}"),
                8 => " ".repeat(1 + rng.below(10)),
                9 => "/".into(),
                10 => format!("{}", rng.next()),
                _ => text.chars().take(rng.below(text.chars().count() + 1)).collect(),
            }
        }
        Value::Number(_) => {
            *value = match rng.below(9) {
                0 => json!(0),
                1 => json!(-1),
                2 => json!(i64::MAX),
                3 => json!(i64::MIN),
                4 => json!(u64::MAX),
                5 => json!(1e300),
                6 => json!(-1e-300),
                7 => json!(0.5),
                _ => json!(rng.next() as i64),
            }
        }
        Value::Bool(flag) => *flag = !*flag,
        _ => {}
    }
}

const SPLICES: &[&[u8]] = &[
    br"\ud800", br"\udc00\ud800", br"\ud83d", br"\u0000", br"\uZZZZ", b"\"", b"{", b"}", b",", b"[", b"]", b"\\",
    b"\xff\xfe", b"\xef\xbb\xbf", b"\xed\xa0\x80", b"\xc0\xaf", b"\x00", b"1e999999", b"-", b"null", b"\r",
];

fn mutate_text(text: String, rng: &mut Rng) -> String {
    let mut bytes = text.into_bytes();
    for _ in 0..=rng.below(4) {
        if bytes.is_empty() {
            break;
        }
        let at = rng.below(bytes.len());
        match rng.below(5) {
            0 => bytes[at] ^= 1 << rng.below(8),
            1 => bytes.truncate(at),
            2 => {
                let end = (at + 1 + rng.below(64)).min(bytes.len());
                let chunk = bytes[at..end].to_vec();
                bytes.splice(at..at, chunk);
            }
            _ => {
                let splice = SPLICES[rng.below(SPLICES.len())];
                bytes.splice(at..at, splice.iter().copied());
            }
        }
    }
    // The stdio reader decodes frames lossily, so handle_line only sees UTF-8.
    String::from_utf8_lossy(&bytes).into_owned()
}

fn frame(seeds: &[Value], tags: &[String], rng: &mut Rng) -> String {
    let mut payload = seeds[rng.below(seeds.len())].clone();
    if rng.chance(55) {
        for _ in 0..=rng.below(3) {
            mutate_typed(&mut payload, rng);
        }
    } else {
        for _ in 0..rng.below(4) {
            mutate(&mut payload, rng, tags, 0);
        }
    }
    let id = if rng.chance(5) { interesting(rng) } else { json!(format!("fuzz-{}", rng.next())) };
    let mut envelope = json!({"id": id, "payload": payload});
    if rng.chance(5) {
        mutate(&mut envelope, rng, tags, 0);
    }
    let text = serde_json::to_string(&envelope).unwrap_or_default();
    if rng.chance(25) { mutate_text(text, rng) } else { text }
}

fn error_kinds(events: &[IpcEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::Error(Wrap { inner }) => Some(inner.kind.clone()),
            _ => None,
        })
        .collect()
}

fn drain(rx: &mut tokio::sync::mpsc::Receiver<IpcEvent>, failures: &mut BTreeMap<String, String>, line: &str) -> Vec<IpcEvent> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    if error_kinds(&out).iter().any(|kind| kind == "command_handler_panic") {
        failures.entry("command_handler_panic event".into()).or_insert_with(|| line.chars().take(400).collect());
    }
    out
}

struct EnvGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl EnvGuard {
    fn set(vars: &[(&'static str, &std::path::Path)]) -> Self {
        let saved = vars.iter().map(|(key, _)| (*key, std::env::var_os(key))).collect();
        for (key, value) in vars {
            std::env::set_var(key, value);
        }
        Self(saved)
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

type PanicLog = Arc<parking_lot::Mutex<BTreeMap<String, usize>>>;

/// Records panics raised on threads whose name starts with `prefix`, including
/// ones a handler catches and turns into an error, and forwards every other
/// panic to the previous hook.
pub(crate) fn record_panics(prefix: &'static str) -> (PanicLog, impl FnOnce()) {
    let log: PanicLog = Arc::default();
    let previous: Arc<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync> = Arc::from(std::panic::take_hook());
    let (hook_log, hook_previous) = (log.clone(), previous.clone());
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name().is_some_and(|name| name.starts_with(prefix)) {
            let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_default();
            *hook_log.lock().entry(format!("{location} {}", message.chars().take(120).collect::<String>())).or_default() += 1;
        } else {
            hook_previous(info);
        }
    }));
    let restore = move || {
        let _ = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| previous(info)));
    };
    (log, restore)
}

pub(crate) fn iterations(var: &str, default: usize) -> usize {
    std::env::var(var).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn seed_catalog(db: &rusqlite::Connection, root: &str) {
    // Rows point at files that do not exist, so trash and rename can never
    // reach a real file even when the fuzzer hits a valid command.
    let _ = db.execute_batch(&format!(
        "INSERT OR IGNORE INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES \
           (1,'{root}/missing/a.jpg',1,100,10,0,'image','jpg'),(2,'{root}/missing/b.txt',2,100,10,0,'doc','txt'),\
           (3,'{root}/missing/c.mov',3,100,10,0,'video','mov'),(42,'{root}/missing/d.pdf',42,100,10,0,'pdf','pdf');\
         INSERT OR IGNORE INTO persons(id,name,created_at) VALUES (1,'Fuzz One',0),(2,'Fuzz Two',0),(3,NULL,0);"
    ));
}

#[test]
fn ipc_fuzzer_never_panics_hangs_or_drops_an_error() {
    let _env_lock = crate::paths::TEST_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = std::env::temp_dir().join(format!("fileid-ipc-fuzz-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&state).unwrap();
    let db_path = state.join("FileID/fileid.sqlite");
    let _env = EnvGuard::set(&[("XDG_DATA_HOME", &state), ("LOCALAPPDATA", &state), ("FILEID_DB", &db_path)]);
    crate::paths::ensure_state_dirs().unwrap();
    let root = state.join("library");
    std::fs::create_dir_all(&root).unwrap();
    let root_text = root.to_string_lossy().replace('\'', "");

    let seed = iterations("FILEID_FUZZ_SEED", 0x5EED_F11E) as u64;
    let total = iterations("FILEID_FUZZ_IPC_ITERATIONS", 3_000);
    let (panics, restore_hook) = record_panics("fileid-fuzz");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("fileid-fuzz-ipc")
        .enable_all()
        .build()
        .unwrap();
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    let mut stats: BTreeMap<&str, usize> = BTreeMap::new();
    let mut dispatched_tags: BTreeSet<String> = BTreeSet::new();

    runtime.block_on(async {
        let writer = db::open_writer(&db_path).expect("temp catalog");
        seed_catalog(&writer, &root_text);
        let (sink, mut events) = Sink::channel_for_test(1 << 16);
        let d = Dispatcher {
            sink,
            shutdown: Arc::new(Notify::new()),
            command_shutdown_requested: Arc::default(),
            db: Some(Arc::new(parking_lot::Mutex::new(writer))),
            catalog_index: None,
            db_path: db_path.clone(),
            scan_state: Arc::default(),
            scan_active: Arc::default(),
            scan_cancel_requested: Arc::default(),
            deep_analyze_cancel: Arc::default(),
            deep_analyze_active: Arc::default(),
            face_cluster_active: Arc::default(),
            restructure_cancellations: RestructureCancelRegistry::default(),
            restructure_active: Arc::default(),
            mutation_gate: Arc::new(tokio::sync::Mutex::new(())),
            jobs: job_queue::JobQueue::new(),
            http_client: crate::downloader::fail_closed_client(),
        };
        let seeds = seeds(&root_text);
        let tags: Vec<String> = seeds.iter().filter_map(|s| s.as_object()?.keys().next().cloned()).collect::<BTreeSet<_>>().into_iter().collect();
        let mut rng = Rng::new(seed);
        let mut recent: std::collections::VecDeque<String> = std::collections::VecDeque::new();

        for iteration in 0..total {
            let line = frame(&seeds, &tags, &mut rng);
            let trimmed = line.trim_start_matches('\u{FEFF}').trim_start();
            if trimmed.is_empty() {
                continue;
            }
            *stats.entry("lines").or_default() += 1;
            let mut decoded: Option<IpcCommand> = serde_json::from_str(trimmed).ok();
            let tag = decoded.as_ref().and_then(|c| serde_json::to_value(&c.payload).ok()).and_then(|v| v.as_object()?.keys().next().cloned());
            let valid = decoded.as_mut().map(|c| ipc::normalize_and_validate_command(&mut c.payload).is_ok());
            match (&tag, valid) {
                (None, _) => *stats.entry("decode_failed").or_default() += 1,
                (Some(_), Some(false)) => *stats.entry("validation_rejected").or_default() += 1,
                _ => {}
            }
            if tag.as_deref().is_some_and(|t| DECODE_ONLY.contains(&t)) && valid == Some(true) {
                *stats.entry("decode_only").or_default() += 1;
                continue;
            }
            // Worker responsiveness: a handler blocking every runtime thread
            // would stall all other commands. This also yields once per frame,
            // as the real stdin loop does on each read.
            let probe = std::time::Instant::now();
            let _ = tokio::spawn(async {}).await;
            if probe.elapsed() > std::time::Duration::from_secs(5) {
                failures.entry("runtime workers starved".into()).or_insert(format!("{:?} after {recent:?}", probe.elapsed()));
            }
            let started = std::time::Instant::now();
            let handled = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(handle_line(&d, &line))),
            )
            .await;
            let sample: String = line.chars().take(400).collect();
            match handled {
                Err(_) => {
                    failures.entry(format!("hang dispatching {}", tag.as_deref().unwrap_or("?"))).or_insert(sample.clone());
                }
                Ok(Err(_)) => {
                    failures.entry(format!("handle_line panicked on {}", tag.as_deref().unwrap_or("?"))).or_insert(sample.clone());
                }
                Ok(Ok(())) => {}
            }
            *stats.entry("dispatched").or_default() += 1;
            if let Some(tag) = &tag {
                dispatched_tags.insert(tag.clone());
            }
            if started.elapsed() > std::time::Duration::from_secs(5) {
                failures
                    .entry(format!("slow dispatch of {}", tag.as_deref().unwrap_or("?")))
                    .or_insert(format!("{:?} after {recent:?}: {sample}", started.elapsed()));
            }
            recent.push_back(format!("{}:{}", tag.as_deref().unwrap_or("?"), line.len()));
            if recent.len() > 12 {
                recent.pop_front();
            }
            let kinds = error_kinds(&drain(&mut events, &mut failures, &line));
            if tag.is_none() && !kinds.iter().any(|k| k == "command_decode_failed") {
                failures.entry("decode failure without command_decode_failed".into()).or_insert(sample.clone());
            }
            if valid == Some(false) && !kinds.iter().any(|k| k == "command_limit_exceeded") {
                failures.entry(format!("rejected {} without command_limit_exceeded", tag.as_deref().unwrap_or("?"))).or_insert(sample);
            }
            if iteration % 200 == 199 {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                if let Some(db) = &d.db {
                    seed_catalog(&db.lock(), &root_text);
                }
            }
        }

        // Every queued mutation must finish: the gate frees within the deadline.
        d.restructure_cancellations.request_cancel();
        d.deep_analyze_cancel.store(true, Ordering::Release);
        match tokio::time::timeout(std::time::Duration::from_secs(60), d.mutation_gate.lock()).await {
            Ok(_guard) => {}
            Err(_) => {
                failures.insert("queued mutations still running 60 s after the last frame".into(), String::new());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        drain(&mut events, &mut failures, "(after the run)");
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    restore_hook();
    for (signature, count) in panics.lock().iter() {
        failures.insert(format!("panic x{count}: {signature}"), String::new());
    }
    let _ = std::fs::remove_dir_all(&state);
    eprintln!("ipc fuzzer: seed {seed:#x}, {stats:?}, dispatched tags {}", dispatched_tags.len());
    assert!(failures.is_empty(), "ipc fuzzer found {} failure signatures:\n{failures:#?}", failures.len());
}

