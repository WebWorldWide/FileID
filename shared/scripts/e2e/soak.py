#!/usr/bin/env python3
"""Soak test for a FileID engine (the shared Rust engine, the Swift engine, or the Rust engine built with the Apple adapters) on a library that changes under it.

usage: python3 soak.py --engine <binary> --flavor rust|swift|hosted --library <generated library dir> --work <scratch dir>
                       [--minutes 45] [--max-rounds N] [--seed S] [--models-dir DIR] [--no-trash]

--library is a directory made by soak_library.py (its <name>.manifest.json sits beside it); it is copied to
<work>/lib and never changed. <work> must be a scratch folder: the engines get FILEID_DB / FILEID_DATABASE_PATH,
XDG_DATA_HOME, HOME, FILEID_MODELS_DIR and FILEID_TEST_TRASH_ROOT inside it, so a run never touches a real
catalog or the real Trash. The engine must be built with the test Trash root (Rust `--features test-trash-root`,
Swift `-Xswiftc -DFILEID_TEST_TRASH_ROOT`); pass --no-trash to skip the Trash steps for a build without it.

One long-lived engine ("main") runs every round; a second engine ("victim") on its own small copy is SIGKILLed and
restarted each round. After every round the script proves: every file not deliberately changed exists with the same
SHA-256 and nothing else appeared; the test Trash is empty; the catalog rows under the library equal the files on
disk (less a fixed, explained set of files the engine never catalogues); PRAGMA integrity_check and foreign_key_check
are clean; no unexpected error event kind arrived; the engine log has no panic, fatal or 'database is locked' line;
the engine still answers healthCheck with the same pid. It records footprint, descriptors, threads, database and WAL
size per round and flags growth. Exit status 0 only when every invariant held in every round."""
import argparse, hashlib, json, os, queue, random, re, shutil, signal, sqlite3, statistics, subprocess, sys, threading, time, uuid

NOTICE_KINDS = {"rescan_no_changes", "empty_folder", "discovery_partial"}
BAD_LOG = re.compile(r"panicked at|\bpanic\b|\bfatal\b|database is locked|SQLITE_BUSY|stack overflow|SIGSEGV|abort", re.I)
FAILURES = []
ERROR_KINDS = {}
ERROR_SAMPLES = {}
LOGBAD = []


def note_sample(k, body):
    msgs = ERROR_SAMPLES.setdefault(k, [])
    if len(msgs) < 3:
        msgs.append(str(body.get("message"))[:300])


def now():
    return time.strftime("%H:%M:%S")


def log(*a):
    print(now(), *a, flush=True)


def fail(round_no, name, detail=""):
    FAILURES.append((round_no, name, str(detail)))
    log(f"FAIL round {round_no}: {name} | {detail}")


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def snapshot(root):
    out = {}
    for dp, dns, fns in os.walk(root, followlinks=False):
        for fn in fns:
            p = os.path.join(dp, fn)
            if not os.path.islink(p):
                out[os.path.relpath(p, root)] = sha(p)
    return out


def inner(p):
    k = next(iter(p))
    v = p[k]
    return v.get("_0", v) if isinstance(v, dict) else v


def kind(p):
    return next(iter(p))


# The driver holds the careful text reader cold (FILEID_OCR_WARMUP=0) so runs do not wait on its first compile. A cold
# reader leaves careful reads owed by design (scan_catchup stage bit 2), so "nothing is still owed" means the tagging
# bit only. --warm-reader lets the reader warm and checks both bits.
OWED_MASK = 1
# A row hidden as "no longer present" (failed = 1) can owe nothing: its file is gone and no pass visits it, so the
# owed counts below leave such rows out.


class Engine:
    """One engine process with an event pump over both stdout and stderr (the Mac-hosted build writes events to
    stderr). Lines that are not JSON events go to <logfile> and are scanned for panics."""

    env_logged = False

    def __init__(self, binary, flavor, state, logfile, trash_root, models, no_trash=False, label="engine"):
        self.label, self.state, self.logfile = label, state, logfile
        os.makedirs(state, exist_ok=True)
        home = os.path.join(state, "home")
        os.makedirs(home, exist_ok=True)
        db = os.path.join(state, "FileID", "fileid.sqlite")
        os.makedirs(os.path.dirname(db), exist_ok=True)
        self.db = db
        env = {k: v for k, v in os.environ.items() if k != "ORT_DYLIB_PATH"}
        env.update({"HOME": home, "CFFIXED_USER_HOME": home, "XDG_DATA_HOME": state, "LOCALAPPDATA": state,
                    "FILEID_MODELS_DIR": models, "FILEID_HF_CACHE_ROOT": os.path.join(state, "hf"),
                    "ORT_DISABLE_TELEMETRY": "1", "TZ": "UTC"})
        if OWED_MASK == 1:
            env["FILEID_OCR_WARMUP"] = "0"
        else:
            env.pop("FILEID_OCR_WARMUP", None)
        env["FILEID_DB" if flavor != "swift" else "FILEID_DATABASE_PATH"] = db
        env["FILEID_DATABASE_PATH" if flavor != "swift" else "FILEID_DB"] = db
        if not no_trash:
            env["FILEID_TEST_TRASH_ROOT"] = trash_root
        assert db.startswith(os.path.abspath(state)), "database must be inside scratch"
        if not Engine.env_logged:
            Engine.env_logged = True
            log("engine environment:", json.dumps({k: env[k] for k in sorted(env) if k.startswith(("FILEID", "ORT", "XDG", "HOME", "CFFIXED", "LOCALAPPDATA", "TZ"))}))
        self.p = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env,
                                  bufsize=0)
        self.q = queue.Queue()
        self.arrivals = []
        self.lock = threading.Lock()
        self.raw = open(logfile, "ab")
        for stream in (self.p.stdout, self.p.stderr):
            threading.Thread(target=self._pump, args=(stream,), daemon=True).start()
        self.ready = self.wait(lambda e: kind(e) == "ready", 120)

    def _pump(self, stream):
        for line in iter(stream.readline, b""):
            try:
                text = line.decode("utf-8", "replace").rstrip("\n")
            except Exception:
                continue
            if text.startswith("{"):
                try:
                    ev = json.loads(text)
                    if isinstance(ev, dict) and isinstance(ev.get("payload"), dict) and ev["payload"]:
                        payload = ev["payload"]
                        if kind(payload) in ("bulkActionResult", "scanComplete", "phaseChanged"):
                            ib = inner(payload)
                            self.arrivals.append((time.time(), kind(payload), ib.get("commandID") if isinstance(ib, dict) else ib))
                        if kind(payload) == "log" or BAD_LOG.search(text):
                            self._note(text)
                        self.q.put(payload)
                        continue
                except ValueError:
                    pass
            self._note(text)

    def _note(self, text):
        with self.lock:
            self.raw.write((text[:2000] + "\n").encode())
            self.raw.flush()
        if BAD_LOG.search(text) and "malformed" not in text.lower() and "invalid" not in text.lower():
            LOGBAD.append((self.label, text[:300]))

    def send(self, name, body=None):
        cid = str(uuid.uuid4())
        line = json.dumps({"id": cid, "payload": {name: body if body is not None else {}}}) + "\n"
        self.write_raw(line.encode())
        self.last_sent = (time.time(), name, cid)
        return cid

    def write_raw(self, data):
        self.p.stdin.write(data)
        self.p.stdin.flush()

    def alive(self):
        return self.p.poll() is None

    def wait(self, pred, timeout=300, on_error=None):
        end = time.time() + timeout
        while time.time() < end:
            try:
                ev = self.q.get(timeout=0.5)
            except queue.Empty:
                if not self.alive():
                    raise RuntimeError(f"{self.label} exited with {self.p.returncode}")
                continue
            if kind(ev) == "error":
                k = inner(ev).get("kind")
                ERROR_KINDS[k] = ERROR_KINDS.get(k, 0) + 1
                note_sample(k, inner(ev))
                if on_error:
                    on_error(ev)
            if pred(ev):
                return ev
        raise TimeoutError(f"{self.label}: no matching event in {timeout}s")

    def drain(self):
        """Discard queued events; returns the scan-terminal event among them (scanComplete or a cancelled/failed phase)
        so a caller that is waiting for one does not lose it."""
        terminal = None
        while True:
            try:
                ev = self.q.get_nowait()
            except queue.Empty:
                return terminal
            if kind(ev) == "error":
                k = inner(ev).get("kind")
                ERROR_KINDS[k] = ERROR_KINDS.get(k, 0) + 1
                note_sample(k, inner(ev))
            elif kind(ev) == "scanComplete" or (kind(ev) == "phaseChanged" and inner(ev) in ("cancelled", "failed")):
                terminal = ev

    def request(self, command, action_key, body, timeout=300):
        rid = str(uuid.uuid4())
        self.send(command, {"request": dict(requestID=rid, **body)} if command in ("catalogRequest", "toolRequest", "chatRequest") else body)
        ev = self.wait(lambda e: (kind(e) == action_key and inner(e).get("requestID") == rid)
                       or (kind(e) == "error" and inner(e).get("kind") not in NOTICE_KINDS), timeout)
        return ev

    def catalog(self, action, timeout=300, **kw):
        rid = str(uuid.uuid4())
        self.send("catalogRequest", {"request": dict(requestID=rid, action=action, **kw)})
        ev = self.wait(lambda e: (kind(e) == "catalogResponse" and inner(e).get("requestID") == rid)
                       or (kind(e) == "error" and inner(e).get("kind") not in NOTICE_KINDS), timeout)
        return inner(ev) if kind(ev) == "catalogResponse" else {"status": "engineError", "message": inner(ev).get("message"), "kind": inner(ev).get("kind")}

    def bulk(self, command, body, timeout=300):
        self.send(command, body)
        ev = self.wait(lambda e: kind(e) == "bulkActionResult" or (kind(e) == "error" and inner(e).get("kind") not in NOTICE_KINDS), timeout)
        return (inner(ev) if kind(ev) == "bulkActionResult" else {"error": inner(ev).get("kind"), "message": inner(ev).get("message"), "succeeded": 0, "failed": -1})

    def health(self, timeout=60):
        rid = str(uuid.uuid4())
        self.send("healthCheck", {"requestID": rid})
        ev = self.wait(lambda e: kind(e) == "healthCheckResult" and inner(e).get("requestID") == rid, timeout)
        return inner(ev)

    def scan(self, root, rescan=False, excluded=None, timeout=900):
        """Returns (terminal kind, payload). Terminal: scanComplete, cancelled, failed (with its error)."""
        body = {"rootPath": root, "rescan": rescan}
        if excluded is not None:
            body["excludedPaths"] = excluded
        self.send("startScan", body)
        return self.wait_scan_end(timeout)

    def wait_scan_end(self, timeout=900):
        seen = {}

        def term(e):
            k = kind(e)
            if k == "scanComplete":
                return True
            if k == "phaseChanged" and inner(e) in ("cancelled", "failed"):
                return True
            return False
        ev = self.wait(term, timeout)
        if kind(ev) == "phaseChanged" and inner(ev) == "failed":
            try:
                err = self.wait(lambda e: kind(e) == "error", 5)
                return "failed", inner(err)
            except TimeoutError:
                return "failed", {}
        if kind(ev) == "scanComplete":
            return "scanComplete", inner(ev)
        if inner(ev) == "cancelled":
            # a cancelled scan still sends its closing scanComplete after phaseChanged(cancelled) (scan_session.rs); take it
            # here so the next scan does not mistake it for its own
            try:
                self.wait(lambda e: kind(e) == "scanComplete", 15)
            except TimeoutError:
                pass
        return inner(ev), {}

    def kill(self):
        if self.alive():
            self.p.kill()
        self.p.wait(timeout=10)

    def close(self):
        rc = None
        try:
            if self.alive():
                try:
                    self.send("shutdown")
                except (BrokenPipeError, OSError):
                    pass
                self.p.wait(timeout=45)
        except subprocess.TimeoutExpired:
            self.p.kill()
            self.p.wait(timeout=10)
        rc = self.p.returncode
        try:
            self.raw.close()
        except Exception:
            pass
        return rc


def proc_metrics(pid, state_dir):
    m = {"pid": pid}
    try:
        out = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True, timeout=60).stdout
        mo = re.search(r"Footprint:\s*([\d.]+)\s*(KB|MB|GB|B)", out)
        if mo:
            m["footprint_mb"] = round(float(mo.group(1)) * {"B": 1 / 1048576, "KB": 1 / 1024, "MB": 1, "GB": 1024}[mo.group(2)], 1)
    except Exception as exc:
        m["footprint_err"] = str(exc)
    try:
        out = subprocess.run(["lsof", "-p", str(pid)], capture_output=True, text=True, timeout=60).stdout
        lines = out.splitlines()[1:]
        m["fds"] = len(lines)
        m["fd_kinds"] = {}
        for ln in lines:
            parts = ln.split()
            if len(parts) > 4:
                m["fd_kinds"][parts[4]] = m["fd_kinds"].get(parts[4], 0) + 1
    except Exception as exc:
        m["fds_err"] = str(exc)
    try:
        out = subprocess.run(["ps", "-M", "-p", str(pid)], capture_output=True, text=True, timeout=30).stdout
        m["threads"] = max(0, len(out.strip().splitlines()) - 1)
    except Exception as exc:
        m["threads_err"] = str(exc)
    try:
        out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True, timeout=30).stdout.strip()
        m["rss_mb"] = round(int(out) / 1024, 1)
    except Exception:
        pass
    db = os.path.join(state_dir, "FileID", "fileid.sqlite")
    for suffix, key in (("", "db_mb"), ("-wal", "wal_mb")):
        try:
            m[key] = round(os.path.getsize(db + suffix) / 1048576, 2)
        except OSError:
            m[key] = 0
    return m


def ro(db):
    return sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=30)


class Soak:
    def __init__(self, a):
        self.a = a
        self.work = os.path.abspath(a.work)
        self.rng = random.Random(a.seed)
        self.lib = os.path.join(self.work, "lib")
        self.trash = os.path.join(self.work, "Trash")
        self.models = a.models_dir or os.path.join(self.work, "models")
        os.makedirs(self.work, exist_ok=True)
        self.report = open(os.path.join(self.work, "soak-report.jsonl"), "a")
        self.series = []
        self.uncatalogued = {}
        self.counter = 0
        self.notes = {}
        self.cycles, self.catchups, self.kills, self.overlap_log, self.heals = [], [], [], [], []

    # ---------- model of the library on disk
    def p(self, rel):
        return os.path.join(self.lib, rel)

    def setup(self):
        for d in (self.work, self.models, self.trash):
            os.makedirs(d, exist_ok=True)
        if os.path.exists(self.lib):
            shutil.rmtree(self.lib)
        shutil.copytree(self.a.library, self.lib, symlinks=True)
        self.model = snapshot(self.lib)
        src_manifest = os.path.join(os.path.dirname(os.path.abspath(self.a.library)), os.path.basename(os.path.abspath(self.a.library)) + ".manifest.json")
        if os.path.exists(src_manifest):
            want = json.load(open(src_manifest))
            if want != self.model:
                fail(0, "library copy matches its manifest", f"{len(set(want) ^ set(self.model))} differing paths")
        log(f"library copy: {len(self.model)} files")
        self.victim_lib = os.path.join(self.work, "victim-lib")
        if os.path.exists(self.victim_lib):
            shutil.rmtree(self.victim_lib)
        keep = sorted(self.model)[::12 if self.a.flavor == "hosted" else 5][:600]
        for rel in keep:
            dst = os.path.join(self.victim_lib, rel)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(self.p(rel), dst)
        self.victim_model = snapshot(self.victim_lib)

    def start_main(self):
        self.main = Engine(self.a.engine, self.a.flavor, os.path.join(self.work, "main"), os.path.join(self.work, "main-engine.log"),
                           self.trash, self.models, self.a.no_trash, "main")
        return self.main

    def db_rows(self, db, prefix):
        with ro(db) as c:
            return {r[0]: (r[1], r[2], r[3], r[4]) for r in c.execute(
                "SELECT path_text, id, failed, kind, error_message FROM files WHERE path_text LIKE ?", (prefix + "/%",))}

    # ---------- invariants
    def check_disk(self, rnd, model, root, name):
        snap = snapshot(root)
        lost = sorted(set(model) - set(snap))
        extra = sorted(set(snap) - set(model))
        altered = sorted(k for k in set(model) & set(snap) if model[k] != snap[k])
        if lost or extra or altered:
            fail(rnd, f"{name}: files on disk equal the expected set",
                 f"lost={lost[:4]}({len(lost)}) extra={extra[:4]}({len(extra)}) altered={altered[:4]}({len(altered)})")
            return False
        return True

    def never_catalogued(self, path, root):
        """Swift only: Discovery uses .skipsHiddenFiles, so a file with a dot-prefixed path component under the root is
        deliberately not catalogued. Such a file must have no visible row (checked by the callers)."""
        return self.a.flavor == "swift" and any(c.startswith(".") for c in os.path.relpath(path, root).split(os.sep))

    @staticmethod
    def tombstone(row):
        """A row the engine keeps but hides after the file vanished: failed=1 with the 'no longer present' message."""
        return bool(row[1]) and "no longer present" in (row[3] or "")

    def check_catalog(self, rnd, eng, root, model, name, first=False):
        """Visible rows (not tombstones) must be exactly the files on disk, except zero-byte files, which the engine
        never catalogues (db/zero_byte.rs: the transition never inserts); tombstones must not point at present files."""
        rows = self.db_rows(eng.db, root)
        disk = {os.path.join(root, r) for r in model}
        visible = {p for p, v in rows.items() if not self.tombstone(v)}
        stale = sorted(visible - disk)
        missing = sorted(p for p in disk - visible if os.path.exists(p) and os.path.getsize(p) != 0 and not self.never_catalogued(p, root))
        hidden_rows = sorted(p for p in visible & disk if self.never_catalogued(p, root))
        if hidden_rows:
            fail(rnd, f"{name}: hidden files are not catalogued (swift)", [os.path.relpath(h, root) for h in hidden_rows[:4]])
        zero_rows = sorted(p for p in visible & disk if os.path.getsize(p) == 0)
        hidden_present = sorted(p for p, v in rows.items() if self.tombstone(v) and p in disk)
        if first:
            log(f"{name}: {len(rows)} rows, {len(disk) - len(visible & disk)} disk files without a visible row (zero-byte: {sum(1 for p in disk - visible if os.path.getsize(p) == 0)})")
        if stale or missing or hidden_present:
            fail(rnd, f"{name}: catalog rows equal files on disk",
                 f"stale visible rows={[os.path.relpath(s, root) for s in stale[:4]]}({len(stale)}) uncatalogued={[os.path.relpath(s, root) for s in missing[:4]]}({len(missing)}) "
                 f"hidden-but-present={[os.path.relpath(s, root) for s in hidden_present[:4]]}({len(hidden_present)})")
        return rows

    @staticmethod
    def classify(path):
        base = os.path.basename(path)
        if "/.hidden-folder/" in path or base.startswith("."):
            return "dot"
        try:
            if os.path.getsize(path) == 0:
                return "zero"
        except OSError:
            pass
        return "other"

    def check_db(self, rnd, db, name):
        with ro(db) as c:
            r = c.execute("PRAGMA integrity_check").fetchall()
            if r != [("ok",)]:
                fail(rnd, f"{name}: integrity_check", r[:3])
            r = c.execute("PRAGMA foreign_key_check").fetchall()
            if r:
                fail(rnd, f"{name}: foreign_key_check", r[:3])

    def check_trash(self, rnd):
        left = []
        for dp, dns, fns in os.walk(self.trash):
            left += [os.path.join(dp, f) for f in fns]
        if left:
            fail(rnd, "test Trash is empty", left[:4])

    def bad_log_since(self, rnd, mark):
        new = LOGBAD[mark:]
        if new:
            fail(rnd, "engine log has no panic/fatal/lock lines", new[:3])
        return len(LOGBAD)

    # ---------- steps
    def ids_by_path(self, eng, rels):
        rows = self.db_rows(eng.db, self.lib)
        return {r: rows[self.p(r)][0] for r in rels if self.p(r) in rows}

    def step_scan(self, rnd, eng):
        t = time.time()
        k, v = eng.scan(self.lib)
        if k != "scanComplete":
            fail(rnd, "scan completes", (k, v))
            return
        # The Rust engine leaves scanned_at alone for an unchanged file. The Swift engine stamps it on every skipped file
        # (Discovery.swift bumpScannedAt: the orphan sweep deletes rows with scanned_at < scanStart), so for swift the
        # check compares every other column, ids included, and requires processedFiles == 0 besides.
        cols = "id, scanned_at" if self.a.flavor != "swift" else "id, size_bytes, modified_at, kind, content_hash, phash, failed, error_message"
        with ro(eng.db) as c:
            before = {r[0]: r[1:] for r in c.execute(f"SELECT {cols} FROM files WHERE path_text LIKE ?", (self.lib + "/%",))}
        k, v = eng.scan(self.lib)
        with ro(eng.db) as c:
            after = {r[0]: r[1:] for r in c.execute(f"SELECT {cols} FROM files WHERE path_text LIKE ?", (self.lib + "/%",))}
        changed = [i for i in set(after) | set(before) if before.get(i) != after.get(i)]
        with ro(eng.db) as c:
            failed_ids = {r[0] for r in c.execute("SELECT id FROM files WHERE failed=1")}
        retried = [i for i in changed if i in failed_ids]
        self.retried_failed = len(retried)
        changed = [i for i in changed if i not in failed_ids]
        if self.a.flavor == "swift" and k == "scanComplete" and v.get("processedFiles"):
            changed.append(f"processedFiles={v.get('processedFiles')}")
        if k != "scanComplete" or changed:
            self.gate_report(rnd, eng, changed)
            fail(rnd, "rescan analyses nothing new", f"{k} reprocessed={len(changed)} {v}")
        return round(time.time() - t, 1)

    def gate_report(self, rnd, eng, changed_ids):
        """Which skip-set gate keeps each re-analysed row out of the skip set (the gate clauses of scan_session.rs), counted by
        kind and extension, from a copy of the catalog."""
        gates = {
            "content_hash": "(content_hash IS NULL AND kind IN ('image','video','pdf','doc','audio'))",
            "text_embed": "(kind IN ('doc','pdf') AND text_stage_done = 0 AND NOT EXISTS (SELECT 1 FROM text_embeddings WHERE text_embeddings.file_id = files.id))",
            "model_clip": "(kind = 'model' AND extension = 'obj' AND text_stage_done = 0 AND NOT EXISTS (SELECT 1 FROM clip_embeddings WHERE clip_embeddings.file_id = files.id))",
            "image_clip": "(kind = 'image' AND NOT EXISTS (SELECT 1 FROM clip_embeddings WHERE clip_embeddings.file_id = files.id))",
            "video_clip": "(kind = 'video' AND NOT EXISTS (SELECT 1 FROM clip_embeddings WHERE clip_embeddings.file_id = files.id))",
        }
        rep = {"round": rnd, "changed": len(changed_ids)}
        try:
            copy = os.path.join(self.work, "gate-copy.sqlite")
            if os.path.exists(copy):
                os.remove(copy)
            with ro(eng.db) as src, sqlite3.connect(copy) as dst:
                src.backup(dst)
            with sqlite3.connect(copy) as c:
                ids = [i for i in changed_ids if isinstance(i, int)]
                marks = ",".join("?" * len(ids)) or "NULL"
                rep["changed_by_kind_ext"] = c.execute(f"SELECT kind, extension, count(*) FROM files WHERE id IN ({marks}) GROUP BY 1,2", ids).fetchall()
                for name, cond in gates.items():
                    rep[name] = {"all_rows": c.execute(f"SELECT count(*) FROM files WHERE failed=0 AND {cond}").fetchone()[0],
                                 "of_changed": c.execute(f"SELECT count(*) FROM files WHERE id IN ({marks}) AND {cond}", ids).fetchone()[0]}
                rep["changed_failed0_in_no_gate"] = c.execute(
                    f"SELECT kind, extension, count(*) FROM files WHERE id IN ({marks}) AND NOT ({' OR '.join(gates.values())}) GROUP BY 1,2", ids).fetchall()
            os.remove(copy)
        except Exception as exc:
            rep["error"] = repr(exc)
        self.notes.setdefault("gate_reports", []).append(rep)
        log("skip-set gate report:", json.dumps(rep))

    def mutate(self, rnd, eng):
        """Change the disk, update the model, rescan and verify the catalog follows."""
        rng = self.rng
        regular = [r for r in sorted(self.model) if self.classify(self.p(r)) == "other" and "Edge/" not in r and os.access(self.p(r), os.W_OK)]
        before_ids = self.ids_by_path(eng, regular)
        deleted, renamed, moved, touched, overwritten, added = [], [], [], [], [], []
        picks = rng.sample(regular, 160)
        for r in picks[:30]:
            os.remove(self.p(r)); del self.model[r]; deleted.append(r)
        for r in picks[30:55]:
            base, ext = os.path.splitext(r)
            new = f"{base}-r{rnd}{ext}"
            os.rename(self.p(r), self.p(new)); self.model[new] = self.model.pop(r); renamed.append((r, new))
        dirs = sorted({os.path.dirname(r) for r in regular})
        for r in picks[55:80]:
            d = rng.choice(dirs)
            new = os.path.join(d, f"m{rnd}-" + os.path.basename(r))
            if os.path.exists(self.p(new)):
                continue
            os.rename(self.p(r), self.p(new)); self.model[new] = self.model.pop(r); moved.append((r, new))
        for r in picks[80:110]:
            t = time.time() - rng.randint(5, 500)  # not in the future: a file dated ahead is re-stamped on every scan until its date passes
            os.utime(self.p(r), (t, t)); touched.append(r)
        for r in picks[110:135]:
            if r.endswith((".txt", ".md", ".csv", ".html")):
                data = (f"overwritten in round {rnd} " + "x" * rng.randint(1, 500) + "\n").encode()
                with open(self.p(r), "wb") as f:
                    f.write(data)
                self.model[r] = hashlib.sha256(data).hexdigest(); overwritten.append(r)
        for i in range(30):
            d = rng.choice(dirs)
            name = f"new-r{rnd}-{i}-{uuid.uuid4().hex[:6]}." + rng.choice(("txt", "md", "csv"))
            data = f"fresh file round {rnd} {i} {uuid.uuid4()} harbor lantern\n".encode()
            rel = os.path.join(d, name)
            os.makedirs(os.path.dirname(self.p(rel)), exist_ok=True)
            with open(self.p(rel), "wb") as f:
                f.write(data)
            self.model[rel] = hashlib.sha256(data).hexdigest(); added.append(rel)
        k, v = eng.scan(self.lib)
        if k != "scanComplete":
            fail(rnd, "scan after disk changes completes", (k, v))
            return
        rows = self.db_rows(eng.db, self.lib)
        gone = [r for r in deleted if self.p(r) in rows and not rows[self.p(r)][1]]
        if gone:
            fail(rnd, "rows for deleted files go", gone[:3])
        for old, new in renamed + moved:
            if self.p(old) in rows and not rows[self.p(old)][1]:
                fail(rnd, "old path of a renamed/moved file has no visible row", old)
            if self.p(new) not in rows:
                fail(rnd, "renamed/moved file has a row at its new path", new)
            elif before_ids.get(old) is not None and rows[self.p(new)][0] != before_ids[old]:
                fail(rnd, "renamed/moved file keeps its catalog identity", f"{old}: id {before_ids[old]} -> {rows[self.p(new)][0]}")
        for r in added:
            if self.p(r) not in rows:
                fail(rnd, "new files appear in the catalog", r)
        for r in overwritten:
            with ro(eng.db) as c:
                row = c.execute("SELECT size_bytes FROM files WHERE path_text=?", (self.p(r),)).fetchone()
            if row is None or row[0] != os.path.getsize(self.p(r)):
                fail(rnd, "overwritten file's size is refreshed", (r, row))
        return dict(deleted=len(deleted), renamed=len(renamed), moved=len(moved), touched=len(touched), overwritten=len(overwritten), added=len(added))

    def step_search(self, rnd, eng):
        words = ["harbor", "lantern", "orchard", "compass", "invoice", "recipe"]
        w = self.rng.choice(words)
        res = eng.catalog("search", query=w, searchMode="keyword", limit=50)
        if res.get("status") != "ok":
            fail(rnd, "keyword search answers ok", res.get("message"))
        else:
            for h in res.get("hits", []):
                if not os.path.exists(h["path"]) and not h["path"].startswith(self.trash):
                    fail(rnd, "search hit points at a file that exists", h["path"])
                    break
        res = eng.catalog("search", query="", searchMode="keyword")
        if res.get("status") == "ok" and res.get("hits"):
            pass
        for action in ("jobs", "listEvents", "autoTakeGroups", "namePreferences"):
            r = eng.catalog(action, limit=20)
            if r.get("status") not in ("ok",):
                fail(rnd, f"catalog {action} answers ok", (r.get("status"), r.get("message")))
        with ro(eng.db) as c:
            fid = c.execute("SELECT id FROM files WHERE path_text LIKE ? AND failed=0 ORDER BY random() LIMIT 1", (self.lib + "/%",)).fetchone()
        if fid:
            r = eng.catalog("detail", fileID=fid[0])
            if r.get("status") != "ok":
                fail(rnd, "detail answers ok", r.get("message"))
        eng.send("listRoots", {"requestID": "lr"})
        eng.wait(lambda e: kind(e) == "rootsList", 30)
        eng.send("listFaceReview", {"requestID": "fr", "queue": "merges"})
        eng.wait(lambda e: kind(e) in ("faceReview", "error"), 30)
        eng.send("findMergeSuggestions")
        eng.wait(lambda e: kind(e) in ("mergeSuggestions", "error"), 30)
        eng.send("markPersonsAsUnknown", {"personIDs": []})
        eng.wait(lambda e: kind(e) in ("bulkActionResult", "error"), 30)
        eng.send("chatRequest", {"request": {"requestID": "c1", "conversationID": "soak", "action": "send", "text": "do I have any duplicate files?"}})
        r = eng.wait(lambda e: kind(e) == "chatResponse" and inner(e).get("status") != "running" or kind(e) == "error", 120)
        if kind(r) == "chatResponse" and inner(r).get("status") != "completed":
            fail(rnd, "chat duplicates question completes", inner(r).get("status"))

    def duplicate_groups(self, eng):
        with ro(eng.db) as c:
            rows = c.execute("""SELECT content_hash, group_concat(id), count(*) FROM files WHERE failed=0 AND content_hash IS NOT NULL
                AND path_text LIKE ? AND size_bytes>0 GROUP BY content_hash HAVING count(*)>1 ORDER BY content_hash""", (self.lib + "/%",)).fetchall()
        return rows

    def step_trash(self, rnd, eng):
        if self.a.no_trash:
            return "skipped"
        groups = self.duplicate_groups(eng)
        if not groups:
            fail(rnd, "library has duplicate groups to trash", "none")
            return
        picked = self.rng.sample(groups, min(5, len(groups)))
        ids = []
        for _, idlist, _ in picked:
            ids.append(int(idlist.split(",")[-1]))
        with ro(eng.db) as c:
            paths = {i: c.execute("SELECT path_text FROM files WHERE id=?", (i,)).fetchone()[0] for i in ids}
        snap = snapshot(self.lib)
        res = eng.bulk("trashFiles", {"fileIDs": ids})
        gone = [p for p in paths.values() if os.path.exists(p)]
        if res.get("succeeded") != len(ids) or gone:
            fail(rnd, "trashFiles moves exactly the chosen duplicates", f"{json.dumps(res)[:240]} still_present={gone[:2]}")
        kept = {os.path.relpath(p, self.lib) for p in paths.values()}
        after = snapshot(self.lib)
        if set(snap) - set(after) != kept and not gone:
            fail(rnd, "only the chosen files left the library", sorted((set(snap) - set(after)) ^ kept)[:4])
        batch = (res.get("action") or "").split(":")[-1]
        left_rows = [i for i in ids if self.rows_with_id(eng, i)]
        if left_rows:
            fail(rnd, "trashed files' rows are removed", left_rows)
        res2 = eng.bulk("restoreFromTrash", {"batchID": batch})
        missing = [p for p in paths.values() if not os.path.exists(p)]
        if missing:
            fail(rnd, "restoreFromTrash puts every file back", f"{json.dumps(res2)[:240]} missing={missing[:2]}")
        self.check_trash(rnd)
        k, v = eng.scan(self.lib)
        if k != "scanComplete":
            fail(rnd, "scan after restore completes", (k, v))
        return len(ids)

    def rows_with_id(self, eng, i):
        with ro(eng.db) as c:
            return c.execute("SELECT 1 FROM files WHERE id=?", (i,)).fetchone()

    def step_rename(self, rnd, eng):
        rows = self.db_rows(eng.db, self.lib)
        cand = [(p, v[0]) for p, v in rows.items() if not v[1] and "/Edge/" not in p and p[len(self.lib) + 1:] in self.model]
        pick = self.rng.sample(cand, 5)
        renames = [{"fileID": i, "newName": f"renamed-r{rnd}-{n}{os.path.splitext(p)[1]}"} for n, (p, i) in enumerate(pick)]
        res = eng.bulk("renameFiles", {"renames": renames})
        new_paths = [os.path.join(os.path.dirname(p), r["newName"]) for (p, i), r in zip(pick, renames)]
        if res.get("succeeded") != 5 or not all(os.path.exists(n) for n in new_paths):
            fail(rnd, "renameFiles renames five files", json.dumps(res)[:240])
        back = [{"fileID": i, "newName": os.path.basename(p)} for p, i in pick]
        res = eng.bulk("renameFiles", {"renames": back})
        if res.get("succeeded") != 5 or not all(os.path.exists(p) for p, _ in pick):
            fail(rnd, "rename undo (rename back) restores the names", json.dumps(res)[:240])
        bad = eng.bulk("renameFiles", {"renames": [{"fileID": pick[0][1], "newName": "../escape.txt"}, {"fileID": 999999999, "newName": "x.txt"}]})
        if os.path.exists(os.path.join(self.lib, "..", "escape.txt")) or bad.get("succeeded"):
            fail(rnd, "renameFiles refuses '../x' and unknown ids", json.dumps(bad)[:200])

    def step_tags(self, rnd, eng):
        with ro(eng.db) as c:
            ids = [r[0] for r in c.execute("SELECT id FROM files WHERE kind='document' AND failed=0 AND path_text LIKE ? LIMIT 3", (self.lib + "/%",))]
        res = eng.bulk("applyTags", {"fileIDs": ids, "tags": ["soak-tag"], "mode": "add"})
        with ro(eng.db) as c:
            n = c.execute("SELECT count(*) FROM tags WHERE tag='soak-tag' AND source='user'").fetchone()[0]
        if res.get("succeeded") == len(ids) and n != len(ids):
            fail(rnd, "applyTags records user tags", f"{res} rows={n}")
        r = eng.catalog("search", query="soak-tag", searchMode="keyword")
        res = eng.bulk("applyTags", {"fileIDs": ids, "tags": ["soak-tag"], "mode": "remove"})
        with ro(eng.db) as c:
            n = c.execute("SELECT count(*) FROM tags WHERE tag='soak-tag' AND source='user'").fetchone()[0]
        if n:
            fail(rnd, "applyTags remove clears the tag", f"{res} rows={n}")

    def step_restructure(self, rnd, eng):
        before = snapshot(self.lib)
        t = time.time()
        eng.send("planRestructure", {"libraryRoot": self.lib})
        ev = eng.wait(lambda e: kind(e) in ("restructurePlan", "error") and (kind(e) != "error" or inner(e).get("kind") not in NOTICE_KINDS), 600)
        if kind(ev) != "restructurePlan":
            fail(rnd, "planRestructure returns a plan", inner(ev))
            return
        plan = inner(ev)
        moves = plan.get("moves") or []
        if snapshot(self.lib) != before:
            fail(rnd, "planRestructure moves nothing", "")
        if not moves:
            return 0
        moves = moves[:400] if rnd % 3 else moves
        self.notes["restructure_moves"] = len(moves)
        eng.send("applyRestructure", {"libraryRoot": self.lib, "moves": moves, "useSymlinks": False})
        ev = eng.wait(lambda e: kind(e) in ("restructureApplyResult", "error") and (kind(e) != "error" or inner(e).get("kind") not in NOTICE_KINDS), 600)
        res = inner(ev)
        self.notes["restructure_result"] = {k: res.get(k) for k in ("applied", "failed", "planned") if isinstance(res, dict)}
        mid = snapshot(self.lib)
        if sorted(mid.values()) != sorted(before.values()):
            fail(rnd, "applyRestructure keeps every file's bytes", f"{len(mid)} vs {len(before)} files; result {json.dumps(res)[:200]}")
        if kind(ev) != "restructureApplyResult" or res.get("failed"):
            fail(rnd, "applyRestructure applies every move", json.dumps(res)[:300])
        eng.send("undoRestructure", {"libraryRoot": self.lib})
        ev = eng.wait(lambda e: kind(e) in ("restructureApplyResult", "error") and (kind(e) != "error" or inner(e).get("kind") not in NOTICE_KINDS), 600)
        after = snapshot(self.lib)
        if after != before:
            fail(rnd, "undoRestructure restores the exact layout", f"lost={len(set(before) - set(after))} extra={len(set(after) - set(before))} {json.dumps(inner(ev))[:200]}")
        empties = [os.path.relpath(os.path.join(rt, d), self.lib) for rt, ds, _ in os.walk(self.lib) for d in ds if not os.listdir(os.path.join(rt, d))]
        self.empties = getattr(self, "empties", None) or set()
        k, v = eng.scan(self.lib)
        if k != "scanComplete":
            fail(rnd, "scan after restructure undo completes", (k, v))
        return len(moves)

    def step_cancel_pause(self, rnd, eng):
        eng.send("startScan", {"rootPath": self.lib, "rescan": True})
        eng.wait(lambda e: kind(e) == "progress", 120)
        eng.send("cancelScan")
        k, v = eng.wait_scan_end(300)
        if k not in ("cancelled", "scanComplete"):
            fail(rnd, "cancelScan ends the scan", (k, v))
        # pause/resume
        eng.send("startScan", {"rootPath": self.lib, "rescan": True})
        eng.wait(lambda e: kind(e) == "progress", 120)
        eng.send("pauseScan")
        time.sleep(1.5)
        done = eng.drain()
        eng.send("resumeScan")
        if done is not None:
            k, v = ("scanComplete", inner(done)) if kind(done) == "scanComplete" else (inner(done), {})
        else:
            k, v = eng.wait_scan_end(600)
        if k != "scanComplete":
            fail(rnd, "paused then resumed scan completes", (k, v))
        # cancel then a normal scan completes with the same catalog
        k, v = eng.scan(self.lib)
        if k != "scanComplete":
            fail(rnd, "scan after cancel completes", (k, v))

    def step_overlap(self, rnd, eng):
        # a mutation during a scan and two scans at once
        rows = self.db_rows(eng.db, self.lib)
        cand = [(p, v[0]) for p, v in rows.items() if not v[1] and "/Edge/" not in p and p[len(self.lib) + 1:] in self.model]
        (p1, i1), = self.rng.sample(cand, 1)
        eng.send("startScan", {"rootPath": self.lib, "rescan": True})
        eng.send("startScan", {"rootPath": self.lib, "rescan": False})
        t_scan = time.time()
        eng.send("renameFiles", {"renames": [{"fileID": i1, "newName": f"during-scan-{rnd}" + os.path.splitext(p1)[1]}]})
        # The first scan holds the engine's scan slot until its scanComplete, and a scan of this library can run
        # longer than any fixed per-event wait on a loaded machine; the next startScan below is only valid once
        # that scanComplete is in, so wait for both events against one overall deadline.
        t_rename, _, rename_id = eng.last_sent
        mark_arr = len(eng.arrivals)
        deadline = time.time() + 600
        got_bulk, completes = False, 0
        newp = os.path.join(os.path.dirname(p1), f"during-scan-{rnd}" + os.path.splitext(p1)[1])
        while time.time() < deadline and not (got_bulk and completes >= 1):
            try:
                ev = eng.wait(lambda e: kind(e) in ("scanComplete", "bulkActionResult") or (kind(e) == "phaseChanged" and inner(e) in ("cancelled", "failed")), 60)
            except TimeoutError:
                continue
            if kind(ev) == "bulkActionResult":
                got_bulk = True
            else:
                completes += 1
        time.sleep(1)
        arr = eng.arrivals[mark_arr:]
        reply = [a for a in arr if a[1] == "bulkActionResult" and a[2] == rename_id]
        completes_t = [a[0] for a in arr if a[1] == "scanComplete"]
        rec = dict(round=rnd, rename_sent=0.0, reply_after_s=round(reply[0][0] - t_rename, 2) if reply else None,
                   scan_completes_after_s=[round(t - t_rename, 2) for t in completes_t], scan_started_before_rename_s=round(t_rename - t_scan, 3))
        self.overlap_log.append(rec)
        log("overlap timing:", json.dumps(rec))
        if completes == 0:
            fail(rnd, "scan during overlapping commands ends", json.dumps(rec))
        if not got_bulk:
            fail(rnd, "rename during a scan gets its reply", json.dumps(rec))
        # whichever way it landed, put the name back and rescan
        cur = self.p1_current(eng, p1, newp)
        rel_old = os.path.relpath(p1, self.lib)
        if cur != p1:
            with ro(eng.db) as c:
                row = c.execute("SELECT id FROM files WHERE path_text=?", (cur,)).fetchone()
            if row is None:
                os.rename(cur, p1) if os.path.exists(cur) else None
            else:
                eng.bulk("renameFiles", {"renames": [{"fileID": row[0], "newName": os.path.basename(p1)}]})
        if not os.path.exists(p1) and os.path.exists(newp):
            os.rename(newp, p1)
        k, v = eng.scan(self.lib)
        if k != "scanComplete":
            fail(rnd, "scan after overlapping commands completes", (k, v))
        # a folder deleted while its scan runs
        eph = os.path.join(self.work, "ephemeral")
        shutil.rmtree(eph, ignore_errors=True)
        for rel in sorted(self.model)[::6][:400]:
            dst = os.path.join(eph, rel)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(self.p(rel), dst)
        eng.send("startScan", {"rootPath": eph, "rescan": True})
        eng.wait(lambda e: kind(e) in ("progress", "discoveryComplete", "phaseChanged"), 60)
        shutil.rmtree(eph)
        try:
            k, v = eng.wait_scan_end(300)
        except TimeoutError:
            fail(rnd, "scan of a deleted folder ends", "timeout")
        eng.send("purgeExcluded", {"excludedPaths": [eph]})
        eng.wait(lambda e: kind(e) in ("bulkActionResult", "error"), 60)
        with ro(eng.db) as c:
            left = c.execute("SELECT count(*) FROM files WHERE path_text LIKE ?", (eph + "/%",)).fetchone()[0]
        if left:
            fail(rnd, "purgeExcluded removes rows of the deleted folder", left)

    def p1_current(self, eng, old, new):
        if os.path.exists(old):
            return old
        return new

    def step_malformed(self, rnd, eng):
        pid = eng.p.pid
        outside = os.path.join(self.work, "outside-library.txt")
        with open(outside, "w") as f:
            f.write("must stay put\n")
        big = b'{"id":"x","payload":{"healthCheck":{"requestID":"' + b"A" * (2 * 1024 * 1024) + b'"}}}\n'
        junk = [b"\n", b"   \n", b"not json at all\n", b"{\n", b'{"id":1,"payload":', b'{"id":"a"}\n', b'{"id":"a","payload":{}}\n',
                b'{"id":"a","payload":{"nope":{}}}\n', b'{"id":"a","payload":{"startScan":{"rootPath":5}}}\n',
                b'{"id":"a","payload":{"startScan":{}}}\n', b'{"id":"a","payload":{"startScan":{"rootPath":"\\u0000"}}}\n',
                b'{"id":"a","payload":{"renameFiles":{"renames":"x"}}}\n', b'{"id":"a","payload":{"trashFiles":{"fileIDs":[-1,1e99,"z"]}}}\n',
                b'{"id":"a","payload":{"restoreFromTrash":{"batchID":"../../../etc"}}}\n', b"\xff\xfe\xfd\xfc\n",
                b'{"id":"a","payload":{"healthCheck":{"requestID":"\xff\xfe"}}}\n', b"\x00\x00\x00\n", b"[" * 5000 + b"\n",
                b'{"a":' * 3000 + b"\n", b'{"id":"a","payload":{"catalogRequest":{"request":{"action":"search","requestID":"r","query":"' + b"%" * 50000 + b'"}}}}\n',
                b'{"id":"a","payload":{"applyTags":{"fileIDs":[1],"tags":[""],"mode":"explode"}}}\n', json.dumps({"id": "a", "payload": {"applyRestructure": {"libraryRoot": self.lib, "moves": [
                    {"fileID": 1, "source": outside, "destination": outside + ".moved", "category": "c"},
                    {"fileID": 2, "source": self.p(next(iter(self.model))), "destination": self.lib + "/../escaped.bin", "category": "c"}],
                    "useSymlinks": False}}}).encode() + b"\n", big]
        for i, line in enumerate(junk):
            eng.write_raw(line if line.endswith(b"\n") else line + b"\n")
            if i % 4 == 0:
                h = eng.health(120)
                if h.get("pid") != pid:
                    fail(rnd, "healthCheck answers between malformed lines", h)
        h = eng.health(120)
        if h.get("pid") != pid or not eng.alive():
            fail(rnd, "engine survives the malformed burst", h)
        if not os.path.exists(outside) or os.path.exists(outside + ".moved") or os.path.exists(os.path.join(self.work, "escaped.bin")):
            fail(rnd, "applyRestructure refuses moves outside the library root", "")
        if snapshot(self.lib) != self.model:
            fail(rnd, "malformed burst changed the library", "")


    # ---------- hosted flavor: the shared Rust engine with the Apple adapters and real models
    def engine_log_lines(self, eng):
        out = []
        logdir = os.path.join(eng.state, "FileID", "logs")
        if os.path.isdir(logdir):
            for fn in sorted(os.listdir(logdir)):
                if fn.startswith("engine.jsonl"):
                    with open(os.path.join(logdir, fn), errors="replace") as f:
                        out += f.read().splitlines()
        return out

    def log_count(self, eng, needle):
        return sum(1 for ln in self.engine_log_lines(eng) if needle in ln)

    def fp(self, eng):
        return proc_metrics(eng.p.pid, eng.state).get("footprint_mb")

    def add_new_images(self, rnd, tag, count, real=0):
        """New files for the library: generated pictures, and `real` copies of a camera JPEG with one byte appended
        (a different hash, still decodable) so the scan has realistic work. Updates the model of the disk."""
        import importlib.util
        spec = importlib.util.spec_from_file_location("soak_library", os.path.join(os.path.dirname(os.path.abspath(__file__)), "soak_library.py"))
        lib = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(lib)
        added = []
        d = f"New/{tag}-r{rnd}"
        os.makedirs(self.p(d), exist_ok=True)
        for i in range(count):
            data = lib.png(self.rng.choice((96, 160, 240)), self.rng.choice((64, 120, 180)), self.rng, tweak=self.rng.randrange(1, 1000))
            rel = f"{d}/gen-{i}-{uuid.uuid4().hex[:6]}.png"
            with open(self.p(rel), "wb") as f:
                f.write(data)
            self.model[rel] = hashlib.sha256(data).hexdigest(); added.append(rel)
        reals = [r for r in sorted(self.model) if r.startswith(self.a.real_dir + "/") and r.lower().endswith(".jpg")]
        for i in range(min(real, len(reals))):
            src = self.rng.choice(reals)
            with open(self.p(src), "rb") as f:
                data = f.read() + uuid.uuid4().bytes
            rel = f"{d}/real-{i}-{uuid.uuid4().hex[:6]}.jpg"
            with open(self.p(rel), "wb") as f:
                f.write(data)
            self.model[rel] = hashlib.sha256(data).hexdigest(); added.append(rel)
        return added

    def new_file_state(self, eng, rels):
        """(files with a row, files with a CLIP embedding, files with tags, failed rows) for the given library files."""
        paths = [self.p(r) for r in rels]
        with ro(eng.db) as c:
            rows = {}
            for pth in paths:
                r = c.execute("SELECT id, failed FROM files WHERE path_text=?", (pth,)).fetchone()
                if r:
                    rows[pth] = r
            ids = [r[0] for r in rows.values()]
            marks = ",".join("?" * len(ids)) or "NULL"
            emb = c.execute(f"SELECT count(*) FROM clip_embeddings WHERE file_id IN ({marks})", ids).fetchone()[0]
            tagged = c.execute(f"SELECT count(DISTINCT file_id) FROM tags WHERE file_id IN ({marks})", ids).fetchone()[0]
        return len(rows), emb, tagged, sum(1 for r in rows.values() if r[1])

    def owed(self, eng):
        with ro(eng.db) as c:
            return c.execute(f"SELECT count(*) FROM scan_catchup WHERE (stages & {OWED_MASK}) != 0 AND file_id IN (SELECT id FROM files WHERE failed = 0 AND path_text LIKE ?)",
                             (self.lib + "/%",)).fetchone()[0]

    def step_real_first(self, eng):
        """Scan the real camera files alone, first, and report aggregate throughput, tags per photo and failures."""
        root = self.p(self.a.real_dir)
        if not os.path.isdir(root):
            return
        n = sum(len(f) for _, _, f in os.walk(root))
        load = os.getloadavg()[0]
        t = time.time()
        k, v = eng.scan(root, timeout=1800)
        dt = time.time() - t
        if k != "scanComplete":
            fail(0, "first scan of the real files completes", (k, v))
            return
        with ro(eng.db) as c:
            rows = c.execute("SELECT count(*), sum(failed), sum(kind='image') FROM files WHERE path_text LIKE ?", (root + "/%",)).fetchone()
            tagged = c.execute("SELECT count(DISTINCT t.file_id), count(*) FROM tags t JOIN files f ON f.id=t.file_id WHERE f.path_text LIKE ? AND f.kind='image'", (root + "/%",)).fetchone()
            emb = c.execute("SELECT count(*) FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE f.path_text LIKE ?", (root + "/%",)).fetchone()[0]
            by_ext = {}
            for pth, failed in c.execute("SELECT path_text, failed FROM files WHERE path_text LIKE ?", (root + "/%",)):
                e = os.path.splitext(pth)[1].lower()
                a = by_ext.setdefault(e, [0, 0]); a[0] += 1; a[1] += failed or 0
        self.notes["real_first_scan"] = dict(files_on_disk=n, rows=rows[0], failed=rows[1], images=rows[2], seconds=round(dt, 1), files_per_s=round(n / dt, 2),
                                             engine_seconds=v.get("totalSeconds"), load=round(load, 1), images_tagged=tagged[0], tag_rows=tagged[1],
                                             tags_per_tagged_image=round(tagged[1] / max(1, tagged[0]), 2), clip_embeddings=emb, by_ext=by_ext)
        log("real files first scan:", json.dumps(self.notes["real_first_scan"]))

    def step_model_cycle(self, rnd, eng):
        """Scan (models load), wait past the idle period (they unload), scan new files (they load again), and search with
        the embeddings. Records the footprint before the wait, after the unload and after the reload."""
        rec = dict(round=rnd)
        added = self.add_new_images(rnd, "cycleA", 6, real=6)
        k, v = eng.scan(self.lib)
        if k != "scanComplete":
            fail(rnd, "scan before the idle wait completes", (k, v))
            return rec
        self.wait_jobs(eng)
        parks0 = self.log_count(eng, "idle model parked")
        loads0 = self.log_count(eng, "Mac scan adapters (MLX)")
        rec["fp_loaded"] = self.fp(eng)
        st = self.new_file_state(eng, added)
        if st[1] != st[0] - st[3]:
            fail(rnd, "every analysed new file has a CLIP embedding", st)
        t = time.time()
        deadline = t + 360
        while time.time() < deadline and self.log_count(eng, "idle model parked") <= parks0:
            time.sleep(5)
        rec["idle_s"] = round(time.time() - t, 1)
        if self.log_count(eng, "idle model parked") <= parks0:
            fail(rnd, "scan models unload after the idle period", f"no 'idle model parked' in {rec['idle_s']}s")
        time.sleep(4)
        rec["fp_unloaded"] = self.fp(eng)
        h = eng.health()
        if h.get("pid") != eng.p.pid:
            fail(rnd, "engine answers while the models are unloaded", h)
        added2 = self.add_new_images(rnd, "cycleB", 6, real=6)
        t = time.time()
        k, v = eng.scan(self.lib)
        rec["reload_scan_s"] = round(time.time() - t, 1)
        if k != "scanComplete":
            fail(rnd, "scan after the unload completes", (k, v))
        rec["fp_reloaded"] = self.fp(eng)
        loads1 = self.log_count(eng, "Mac scan adapters (MLX)")
        rec["adapter_loads"] = loads1 - loads0
        ok_lines = [ln for ln in self.engine_log_lines(eng) if "Mac scan adapters (MLX)" in ln][loads0:]
        if loads1 <= loads0 or not all('"clip":true' in ln and '"ram_plus":true' in ln for ln in ok_lines):
            fail(rnd, "scan models load again on MLX after the unload", ok_lines[-2:])
        st = self.new_file_state(eng, added2)
        rec["new_after_reload"] = st
        if st[1] != st[0] - st[3] or st[0] != len(added2):
            fail(rnd, "files scanned after the reload get rows and CLIP embeddings", st)
        self.step_embedding_search(rnd, eng, rec)
        self.cycles.append(rec)
        log("model cycle:", json.dumps(rec))
        return rec

    def wait_jobs(self, eng, timeout=240):
        """Background jobs a scan starts by itself (catalogIndex, takeGrouping) run to a terminal state before the idle wait,
        so the models' idle period is not extended by them and their effect is part of what the round measures."""
        end = time.time() + timeout
        seen = set()
        while time.time() < end:
            jobs = [j for j in (eng.catalog("jobs", limit=50).get("jobs") or []) if j.get("kind") in ("catalogIndex", "takeGrouping")]
            seen |= {j.get("kind") for j in jobs}
            if all(j.get("state") in ("completed", "failed", "cancelled") for j in jobs):
                for k in seen:
                    self.notes.setdefault("jobs_seen", {})[k] = self.notes.get("jobs_seen", {}).get(k, 0) + 1
                return
            time.sleep(2)
        self.notes["job_wait_timeouts"] = self.notes.get("job_wait_timeouts", 0) + 1

    def step_embedding_search(self, rnd, eng, rec):
        """The engine takes the query vector from its client (the text tower runs in the app and needs ONNX Runtime, which
        this Mac does not have), so the vector here is a stored image embedding: searching with a file's own vector must
        find that file (or a byte-identical twin) first, in semantic mode, in hybrid mode with a keyword, and by seed file."""
        import struct
        # the visual index is built by a background job after a scan; similarity answers "preparing" until it is done
        end = time.time() + 180
        while time.time() < end:
            jobs = [j for j in (eng.catalog("jobs", limit=50).get("jobs") or []) if j.get("kind") in ("catalogIndex", "takeGrouping")]
            if jobs and all(j.get("state") in ("completed", "failed") for j in jobs):
                break
            time.sleep(2)
        with ro(eng.db) as c:
            picks = c.execute("SELECT f.id, f.path_text, e.model, e.embedding FROM clip_embeddings e JOIN files f ON f.id=e.file_id "
                              "WHERE f.failed=0 AND f.path_text LIKE ? ORDER BY random() LIMIT 3", (self.lib + "/%",)).fetchall()
        out = []
        for fid, pth, model, blob in picks:
            vec = list(struct.unpack("<512f", blob))
            with ro(eng.db) as c:
                twins = {r[0] for r in c.execute("SELECT id FROM files WHERE content_hash=(SELECT content_hash FROM files WHERE id=?) AND failed=0", (fid,))}
            word = os.path.splitext(os.path.basename(pth))[0].split("-")[0][:20] or "photo"
            for mode, kw in (("semantic", dict(queryVector=vec, embeddingModel=model)), ("hybrid", dict(query=word, queryVector=vec, embeddingModel=model)),
                             ("semantic", dict(fileID=fid))):
                res = eng.catalog("search", searchMode=mode, limit=20, **kw)
                for _ in range(60):
                    # the first similarity request after new embeddings starts the index job and is answered "preparing"
                    if "Preparing the local visual search index" not in str(res.get("message")):
                        break
                    time.sleep(2)
                    res = eng.catalog("search", searchMode=mode, limit=20, **kw)
                tag = f"{mode}{'-seed' if 'fileID' in kw else ''}"
                if res.get("status") != "ok":
                    if mode == "hybrid" and "ONNX Runtime" in str(res.get("message")):
                        # hybrid ranking loads ONNX Runtime for its text half; this Mac has none (a shipped app bundles it)
                        self.notes["hybrid_search_needs_onnxruntime"] = self.notes.get("hybrid_search_needs_onnxruntime", 0) + 1
                        continue
                    fail(rnd, f"{tag} search answers", (res.get("status"), res.get("message")))
                    continue
                hits = res.get("hits") or []
                for h in hits:
                    if not os.path.exists(h["path"]):
                        fail(rnd, f"{tag} search hit points at a file that exists", h["path"]); break
                top = [h["fileID"] for h in hits[:5]]
                if mode == "semantic" and "fileID" not in kw and not (twins & set(top)):
                    fail(rnd, f"{tag} search with a file's own vector finds it in the top 5", (fid, top))
                out.append((tag, len(hits)))
        rec["embedding_searches"] = out
        res = eng.catalog("search", query="a photo of people outdoors", searchMode="hybrid", limit=5)
        rec["hybrid_text_only"] = (res.get("status"), (res.get("message") or "")[:80])
        # a hybrid query without a vector is refused by contract (the client supplies it); it must be refused, not crash
        if eng.health().get("pid") != eng.p.pid:
            fail(rnd, "engine survives a hybrid search without a vector", "")

    def step_catchup_stop(self, rnd, eng):
        """A scan of new camera files that is stopped in its after-scan tagging pass leaves a consistent catalog, and the
        next scan finishes the tagging it still owed."""
        added = self.add_new_images(rnd, "catchup", 4, real=20)
        eng.drain()
        eng.send("startScan", {"rootPath": self.lib, "rescan": False})
        seen_post = False
        try:
            eng.wait(lambda e: kind(e) == "phaseChanged" and inner(e) == "postScan", 600)
            seen_post = True
        except TimeoutError:
            fail(rnd, "scan reaches its after-scan tagging pass", "no postScan phase in 600s")
        except RuntimeError:
            raise
        eng.send("cancelScan")
        k, v = eng.wait_scan_end(300)
        owed_after_stop = self.owed(eng)
        self.check_db(rnd, eng.db, "main (after a stop in the after-scan pass)")
        if k not in ("cancelled", "scanComplete"):
            fail(rnd, "a stop during the after-scan pass ends the scan", (k, v))
        st = self.new_file_state(eng, added)
        if st[0] != len(added):
            fail(rnd, "files scanned before the stop have rows", st)
        k2, v2 = eng.scan(self.lib)
        if k2 != "scanComplete":
            fail(rnd, "the scan after a stop completes", (k2, v2))
        owed_end = self.owed(eng)
        if owed_end:
            fail(rnd, "the next scan finishes what the after-scan pass owed", owed_end)
        st2 = self.new_file_state(eng, added)
        if st2[1] != st2[0] - st2[3]:
            fail(rnd, "files of a stopped scan get their CLIP embeddings by the next scan", st2)
        rec = dict(round=rnd, saw_postscan=seen_post, stop_result=k, owed_after_stop=owed_after_stop, owed_end=owed_end, new_state=st2)
        self.catchups.append(rec)
        log("catch-up stop:", json.dumps(rec))
        return rec

    def step_victim_phase(self, rnd):
        """SIGKILL an engine on a fresh catalog during its model load, its tagging pass or its after-scan pass (rotating),
        restart it on the same catalog, rescan twice and verify."""
        phase = ("load", "tagging", "postscan")[(rnd - 1) % 3]
        state = os.path.join(self.work, "victim")
        shutil.rmtree(state, ignore_errors=True)
        mk = lambda: Engine(self.a.engine, self.a.flavor, state, os.path.join(self.work, "victim-engine.log"), self.trash, self.models, self.a.no_trash, "victim")
        v = mk()
        killed_at = None
        try:
            v.drain()
            v.send("startScan", {"rootPath": self.victim_lib, "rescan": True})
            t = time.time()
            if phase == "load":
                # the first progress event comes after the stack has loaded; kill while the load is running
                end = t + 0.05 + self.rng.random() * 0.65
                while time.time() < end and v.alive():
                    time.sleep(0.05)
                killed_at = "after %.1fs" % (time.time() - t)
            elif phase == "tagging":
                try:
                    v.wait(lambda e: kind(e) == "progress" and (inner(e).get("processed") or 0) >= 1 and inner(e).get("phase") == "tagging", 120)
                    killed_at = "tagging, processed>=1"
                except TimeoutError:
                    killed_at = "no tagging progress seen"
            else:
                try:
                    v.wait(lambda e: kind(e) == "phaseChanged" and inner(e) == "postScan", 300)
                    time.sleep(self.rng.random() * 2)
                    killed_at = "postScan"
                except TimeoutError:
                    killed_at = "no postScan seen"
            v.kill()
        finally:
            v.raw.close()
        v = mk()
        try:
            k, res = v.scan(self.victim_lib)
            if k != "scanComplete":
                fail(rnd, f"scan after SIGKILL during {phase} and restart completes", (k, res))
            self.check_db(rnd, v.db, f"victim ({phase})")
            self.check_disk(rnd, self.victim_model, self.victim_lib, "victim library")
            k, res = v.scan(self.victim_lib)
            with ro(v.db) as c:
                failed_rows = c.execute("SELECT count(*) FROM files WHERE failed=1").fetchone()[0]
            if k != "scanComplete" or (res.get("processedFiles") or 0) > failed_rows:
                fail(rnd, f"second scan after the {phase} kill analyses nothing but retries of failed files", (k, res, failed_rows))
            rows = self.db_rows(v.db, self.victim_lib)
            disk = {os.path.join(self.victim_lib, r) for r in self.victim_model}
            stale = {p for p, x in rows.items() if not self.tombstone(x)} - disk
            uncovered = [d for d in disk - set(rows) if os.path.getsize(d) != 0]
            if stale or uncovered:
                fail(rnd, f"victim catalog equals its files after the {phase} kill", (len(stale), len(uncovered)))
            with ro(v.db) as c:
                owed = c.execute(f"SELECT count(*) FROM scan_catchup WHERE (stages & {OWED_MASK}) != 0 AND file_id IN (SELECT id FROM files WHERE failed = 0)").fetchone()[0]
                noemb = c.execute("SELECT count(*) FROM files f WHERE kind='image' AND failed=0 AND size_bytes>0 AND NOT EXISTS (SELECT 1 FROM clip_embeddings e WHERE e.file_id=f.id)").fetchone()[0]
            if owed:
                fail(rnd, f"nothing is still owed after the {phase} kill and two scans", owed)
            self.kills.append(dict(round=rnd, phase=phase, killed=killed_at, images_without_embedding=noemb))
            log("victim kill:", json.dumps(self.kills[-1]))
        finally:
            v.close()

    def step_heal(self, rnd, cancel_first):
        """After a SIGKILL during a scan, the first full scan of the folder heals (reprocesses the face-bearing files whose
        crops are missing) and the next scan reprocesses nothing. With cancel_first, the first scan is cancelled and the heal
        must still happen on the following full scan."""
        state = os.path.join(self.work, "heal")
        shutil.rmtree(state, ignore_errors=True)
        hl = os.path.join(self.work, "heal-lib")
        shutil.rmtree(hl, ignore_errors=True)
        os.makedirs(hl)
        reals = [r for r in sorted(self.model) if r.startswith(self.a.real_dir + "/") and r.lower().endswith(".jpg")][:: 2][:30]
        for i, r in enumerate(reals):
            shutil.copy2(self.p(r), os.path.join(hl, f"{i}.jpg"))
        mk = lambda: Engine(self.a.engine, self.a.flavor, state, os.path.join(self.work, "heal-engine.log"), self.trash, self.models, self.a.no_trash, "heal")
        v = mk()
        try:
            k, res = v.scan(hl)
        finally:
            v.close()
        # The crash must leave a 'running' scan_sessions row: that row is what makes the next start an unclean one. A kill
        # at the first phaseChanged raced the row's insert, and a kill after seeing the row can still land after the scan
        # ended, so look for the row after the kill and try again when it is not there.
        crash_seen = False
        for attempt in range(6):
            v = mk()
            try:
                v.drain()
                v.send("startScan", {"rootPath": hl, "rescan": True})
                end = time.time() + 30
                while time.time() < end and v.alive():
                    with ro(v.db) as c:
                        if c.execute("SELECT count(*) FROM scan_sessions WHERE status='running'").fetchone()[0]:
                            break
                    time.sleep(0.005)
                v.kill()
            finally:
                v.raw.close()
            with ro(v.db) as c:
                crash_seen = c.execute("SELECT count(*) FROM scan_sessions WHERE status='running'").fetchone()[0] > 0
            if crash_seen:
                break
        # What a kill leaves behind when it falls between a face batch's commit and its crop writes: face rows with no crop
        # file. The rescan above reprocesses nothing on an already scanned folder, and the Mac path writes crops after the
        # commit, so a real kill rarely lands in that window and the generated library has no faces; make the state explicit.
        crops_dir = os.path.join(state, "FileID", "face_crops")
        db = sqlite3.connect(v.db)
        try:
            have = {r[0] for r in db.execute("SELECT DISTINCT file_id FROM face_prints")}
            images = [r[0] for r in db.execute("SELECT id FROM files WHERE kind='image' AND failed=0 ORDER BY id") if r[0] not in have][:12]
            for fid in images:
                db.execute("INSERT INTO face_prints (file_id, print_data, bbox) VALUES (?, ?, ?)", (fid, b"\0" * 8, "[0,0,1,1]"))
            db.commit()
            owed_files = {r[0] for r in db.execute("SELECT file_id, id FROM face_prints")}
            for _, face_id in db.execute("SELECT file_id, id FROM face_prints"):
                try:
                    os.remove(os.path.join(crops_dir, f"{face_id}.jpg"))
                except FileNotFoundError:
                    pass
        finally:
            db.close()
        v = mk()
        try:
            with ro(v.db) as c:
                faces = c.execute("SELECT count(DISTINCT file_id) FROM face_prints").fetchone()[0]
            rec = dict(round=rnd, cancel_first=cancel_first, face_files=faces, injected_face_files=len(images), crash_row_seen=crash_seen, kill_attempts=attempt + 1)
            if cancel_first:
                v.send("startScan", {"rootPath": hl, "rescan": False})
                time.sleep(0.05)
                v.send("cancelScan")
                k, res = v.wait_scan_end(120)
                rec["first"] = [k, res.get("processedFiles")]
            k, res = v.scan(hl)
            rec["full"] = [k, res.get("processedFiles")]
            k, res = v.scan(hl)
            rec["next"] = [k, res.get("processedFiles")]
            k3, res3 = v.scan(hl)
            rec["next2"] = [k3, res3.get("processedFiles")]
            self.heals.append(rec)
            log("heal check:", json.dumps(rec))
            with ro(v.db) as c:
                failed_rows = c.execute("SELECT count(*) FROM files WHERE failed=1").fetchone()[0]
            if (rec["next"][1] or 0) > failed_rows or (rec["next2"][1] or 0) > failed_rows:
                fail(rnd, "the scans after the healing scan reprocess nothing", rec)
            if not crash_seen:
                fail(rnd, "the heal check's kill left a running scan session", rec)
            if (rec["full"][1] or 0) != len(owed_files):
                fail(rnd, "the first scan after a crash heals exactly the files with face rows and no crop" if not cancel_first
                     else "a cancelled first scan after a crash does not lose the heal", dict(rec, expected=len(owed_files)))
            self.check_db(rnd, v.db, "heal")
        finally:
            v.close()

    def step_victim(self, rnd):
        if self.a.flavor == "hosted":
            return self.step_victim_phase(rnd)
        return self.step_victim_plain(rnd)

    def step_victim_plain(self, rnd):
        """SIGKILL an engine in the middle of a scan, restart it on the same catalog, rescan."""
        v = Engine(self.a.engine, self.a.flavor, os.path.join(self.work, "victim"), os.path.join(self.work, "victim-engine.log"),
                   self.trash, self.models, self.a.no_trash, "victim")
        try:
            v.send("startScan", {"rootPath": self.victim_lib, "rescan": True})
            time.sleep(self.rng.uniform(0.2, 2.0))
            v.kill()
        finally:
            v.raw.close()
        v = Engine(self.a.engine, self.a.flavor, os.path.join(self.work, "victim"), os.path.join(self.work, "victim-engine.log"),
                   self.trash, self.models, self.a.no_trash, "victim")
        try:
            k, res = v.scan(self.victim_lib)
            if k != "scanComplete":
                fail(rnd, "scan after SIGKILL and restart completes", (k, res))
            self.check_db(rnd, v.db, "victim")
            self.check_disk(rnd, self.victim_model, self.victim_lib, "victim library")
            rows = self.db_rows(v.db, self.victim_lib)
            disk = {os.path.join(self.victim_lib, r) for r in self.victim_model}
            stale = {p for p, v in rows.items() if not self.tombstone(v)} - disk
            if stale:
                fail(rnd, "victim catalog has no rows for missing files", sorted(stale)[:3])
            uncovered = [d for d in disk - set(rows) if os.path.getsize(d) != 0 and not self.never_catalogued(d, self.victim_lib)]
            if uncovered:
                fail(rnd, "victim catalog covers its files", len(uncovered))
            k, res = v.scan(self.victim_lib)
            if k != "scanComplete":
                fail(rnd, "second scan after restart completes", (k, res))
        finally:
            v.close()

    # ---------- the rounds
    def round(self, rnd, eng, steps):
        marks = len(LOGBAD)
        timing = {}
        for name, fn in steps:
            t = time.time()
            try:
                out = fn()
            except (TimeoutError, RuntimeError, BrokenPipeError) as exc:
                fail(rnd, f"step {name} ran to completion", repr(exc))
                out = None
                if not eng.alive():
                    raise
            timing[name] = round(time.time() - t, 1)
        # invariants
        t0 = time.time()
        self.check_disk(rnd, self.model, self.lib, "library")
        self.check_trash(rnd)
        eng.drain()
        self.check_catalog(rnd, eng, self.lib, self.model, "main", first=(rnd == 1))
        self.check_db(rnd, eng.db, "main")
        h = eng.health()
        if h.get("pid") != eng.p.pid:
            fail(rnd, "engine answers healthCheck with its pid", h)
        self.bad_log_since(rnd, marks)
        m = proc_metrics(eng.p.pid, os.path.join(self.work, "main"))
        with ro(eng.db) as c:
            m["hidden_rows"] = c.execute("SELECT count(*) FROM files WHERE failed=1 AND error_message LIKE '%no longer present%'").fetchone()[0]
            m["file_rows"] = c.execute("SELECT count(*) FROM files").fetchone()[0]
        m.update(round=rnd, notes=dict(self.notes), retried_failed=getattr(self, "retried_failed", None), steps=timing, check_s=round(time.time() - t0, 1), errors=dict(ERROR_KINDS), load=os.getloadavg()[0])
        self.series.append(m)
        self.report.write(json.dumps(m) + "\n")
        self.report.flush()
        log(f"round {rnd} done: footprint={m.get('footprint_mb')}MB fds={m.get('fds')} threads={m.get('threads')} db={m['db_mb']}MB wal={m['wal_mb']}MB hidden={m['hidden_rows']} rows={m['file_rows']} steps={timing}")

    def run(self):
        a = self.a
        self.setup()
        eng = self.start_main()
        log("engine ready:", json.dumps(eng.ready)[:200])
        if a.flavor == "hosted":
            self.step_real_first(eng)
        t_start = time.time()
        rnd = 0
        deadline = t_start + a.minutes * 60
        while True:
            rnd += 1
            stats = {}
            steps = [
                ("scan+rescan", lambda: self.step_scan(rnd, eng)),
                ("mutate", lambda: stats.update(mut=self.mutate(rnd, eng))),
                ("search", lambda: self.step_search(rnd, eng)),
                ("trash", lambda: self.step_trash(rnd, eng)),
                ("rename", lambda: self.step_rename(rnd, eng)),
                ("tags", lambda: self.step_tags(rnd, eng)),
                ("restructure", lambda: self.step_restructure(rnd, eng)),
                ("cancel/pause", lambda: self.step_cancel_pause(rnd, eng)),
                ("overlap", lambda: self.step_overlap(rnd, eng)),
                ("malformed", lambda: self.step_malformed(rnd, eng)),
                ("victim-kill", lambda: self.step_victim(rnd)),
            ]
            if a.flavor == "hosted":
                if rnd <= 2:
                    steps.append(("crash-heal", lambda: self.step_heal(rnd, cancel_first=(rnd == 2))))
                steps.insert(3, ("catchup-stop", lambda: self.step_catchup_stop(rnd, eng)))
                if rnd % a.cycle_every == 0 or rnd == 1:
                    steps.insert(4, ("model-cycle", lambda: self.step_model_cycle(rnd, eng)))
            try:
                self.round(rnd, eng, steps)
            except Exception as exc:
                fail(rnd, "round aborted", repr(exc))
                if not eng.alive():
                    log("main engine died; restarting on the same catalog")
                    eng = self.start_main()
            if (a.max_rounds and rnd >= a.max_rounds) or time.time() > deadline:
                break
        # final: SIGKILL the long-lived engine, restart on the same catalog and verify everything again
        log("final: SIGKILL main engine mid-scan, restart on the same catalog")
        eng.send("startScan", {"rootPath": self.lib, "rescan": True})
        time.sleep(1.0)
        eng.kill()
        eng.raw.close()
        eng = self.start_main()
        k, v = eng.scan(self.lib)
        if k != "scanComplete":
            fail(rnd, "scan after SIGKILL of the long-lived engine completes", (k, v))
        k, v = eng.scan(self.lib)
        self.check_disk(rnd, self.model, self.lib, "library (final)")
        self.check_catalog(rnd, eng, self.lib, self.model, "main")
        self.check_db(rnd, eng.db, "main (final)")
        rc = eng.close()
        if rc not in (0, None):
            fail(rnd, "engine exits cleanly on shutdown", rc)
        self.summarize(rnd, time.time() - t_start)

    def summarize(self, rounds, seconds):
        s = self.series
        log(f"== {rounds} rounds in {seconds / 60:.1f} min")
        def col(key):
            return [x[key] for x in s if key in x]
        for key in ("footprint_mb", "rss_mb", "fds", "threads", "db_mb", "wal_mb", "hidden_rows", "file_rows"):
            c = col(key)
            if c:
                log(f"{key}: first={c[0]} median={statistics.median(c)} last={c[-1]} (round 3={c[2] if len(c) > 2 else '-'})")
        mem = col("footprint_mb")
        if len(mem) >= 12:
            # one footprint sample swings with the Mac's memory compression and load; compare quartile medians
            q = max(3, len(mem) // 4)
            early, late = statistics.median(mem[2:2 + q]), statistics.median(mem[-q:])
            log(f"footprint median of rounds 3..{2 + q}: {early} MB, of the last {q} rounds: {late} MB")
            if late > early * 1.10:
                fail(rounds, "memory growth from the early to the late rounds under 10% (median of a quarter each)", f"{early} -> {late} MB")
        elif len(mem) > 3 and mem[2] and mem[-1] > mem[2] * 1.10:
            fail(rounds, "memory growth from round 3 to the last round under 10%", f"{mem[2]} -> {mem[-1]} MB")
        for key in ("fds", "threads"):
            c = col(key)
            if len(c) > 5 and c[-1] > max(c[2:5]) + max(2, 0.1 * max(c[2:5])) and c[-1] >= c[len(c) // 2] >= c[2]:
                fail(rounds, f"{key} not growing", f"{c[:6]} ... {c[-3:]}")
        if self.cycles:
            log("unload/reload footprint series (MB):")
            for c in self.cycles:
                log("  ", json.dumps({k: c.get(k) for k in ("round", "fp_loaded", "fp_unloaded", "fp_reloaded", "idle_s", "reload_scan_s", "adapter_loads")}))
            fl = [c["fp_unloaded"] for c in self.cycles if c.get("fp_unloaded")]
            if len(fl) >= 3 and fl[-1] > fl[0] * 1.10 and fl[-1] >= max(fl[:-1]):
                fail(rounds, "footprint after the unload does not climb from cycle to cycle", fl)
        log("overlap timings:", json.dumps(self.overlap_log))
        log("heal checks:", json.dumps(self.heals))
        log("kills:", json.dumps(self.kills))
        log("catch-up stops:", json.dumps(self.catchups))
        log("error kinds seen:", json.dumps(ERROR_KINDS))
        for k, v in ERROR_SAMPLES.items():
            log(f"  sample {k}: {v}")
        log(f"failures: {len(FAILURES)}")
        for f in FAILURES:
            log("  ", f)
        json.dump({"overlap": self.overlap_log, "heals": self.heals, "cycles": self.cycles, "catchups": self.catchups, "kills": self.kills, "rounds": rounds, "seconds": seconds, "error_kinds": ERROR_KINDS, "error_samples": ERROR_SAMPLES, "failures": FAILURES, "series": s},
                  open(os.path.join(self.work, "soak-summary.json"), "w"), indent=1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", required=True)
    ap.add_argument("--flavor", choices=("rust", "swift", "hosted"), required=True)
    ap.add_argument("--library", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--minutes", type=float, default=45)
    ap.add_argument("--max-rounds", type=int, default=0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--models-dir")
    ap.add_argument("--no-trash", action="store_true")
    ap.add_argument("--warm-reader", action="store_true", help="let the careful text reader warm up and require careful reads to be caught up too")
    ap.add_argument("--cycle-every", type=int, default=2, help="hosted: every Nth round, wait for the idle unload of the scan models and scan again")
    ap.add_argument("--real-dir", default="Real", help="hosted: library subfolder of real camera files; scanned alone first and timed")
    a = ap.parse_args()
    if a.warm_reader:
        global OWED_MASK
        OWED_MASK = 3
    work = os.path.abspath(a.work)
    if not (a.no_trash or os.path.isabs(os.path.join(work, "Trash"))):
        sys.exit("bad work dir")
    s = Soak(a)
    try:
        s.run()
    finally:
        for p in subprocess.run(["pgrep", "-f", os.path.abspath(a.engine)], capture_output=True, text=True).stdout.split():
            pass
    sys.exit(1 if FAILURES else 0)


if __name__ == "__main__":
    main()
