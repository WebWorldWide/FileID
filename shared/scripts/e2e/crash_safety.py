#!/usr/bin/env python3
"""Crash-safety and concurrency harness for the macOS Swift engine (and the Rust engine with --rust).

Every experiment builds a fresh library of APFS clones of the photos in FILEID_CRASH_SRC (never the
originals) under FILEID_CRASH_DIR/crash-<name>-<ms>, records the SHA-256 of every file, does
something destructive (SIGKILL mid-command, overlapping commands, the root vanishing mid-scan),
restarts the engine on the same database and checks:
  * every original file exists exactly once with the same bytes, or sits in the Trash where the
    engine's Trash journal can put it back;
  * the catalog has no rows for missing files and no lost rows for present files;
  * the engine starts cleanly and any journal/undo still works.

The engine always runs with FILEID_DATABASE_PATH (Swift) / FILEID_DB (Rust) inside the experiment
directory. trashFiles uses the real macOS Trash: every scratch file name carries the experiment tag,
so anything left behind is easy to find (`ls ~/.Trash | grep <tag>`); restore_all() puts it back.

usage: FILEID_CRASH_DIR=<scratch> FILEID_CRASH_SRC=<dir of photos> FILEID_ENGINE=<FileIDEngine>
       python3 crash_safety.py [experiment ...]      (no argument = all)
"""
import glob, hashlib, json, os, random, signal, sqlite3, subprocess, sys, threading, time, uuid
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from drive import Engine, kind

ROOT = os.path.abspath(os.environ.get("FILEID_CRASH_DIR") or sys.exit("set FILEID_CRASH_DIR"))
RUST = os.environ.get("FILEID_RUST_ENGINE")  # set to the Rust FileIDEngine to drive the shared engine instead
SRC = os.path.abspath(os.environ.get("FILEID_CRASH_SRC") or sys.exit("set FILEID_CRASH_SRC"))
TRASH = os.path.expanduser("~/.Trash")
COUNT = int(os.environ.get("FILEID_CRASH_COUNT", "60"))
RESULTS = []


def inner(p, k=None):
    k = k or kind(p)
    v = p[k]
    return v.get("_0", v) if isinstance(v, dict) else v


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


LABS = []


class Lab:
    """A scratch library of clones plus its isolated engine state directory."""

    def __init__(self, name, count=COUNT):
        LABS.append(self)
        stamp = str(int(time.time() * 1000))[-9:]
        self.tag = f"fidcrash{stamp}"
        self.dir = os.path.join(ROOT, f"crash-{name}-{stamp}")
        self.lib = os.path.join(self.dir, "lib")
        self.state = os.path.join(self.dir, "db")
        self.out = os.path.join(self.dir, "out")
        for d in (self.lib + "/Trip", self.lib + "/Misc", self.state, self.out):
            os.makedirs(d)
        photos = sorted(f for f in os.listdir(SRC) if f.lower().endswith((".jpg", ".jpeg")))[:count]
        half = len(photos) // 2
        for i, f in enumerate(photos):
            sub = "Trip" if i < half else "Misc"
            subprocess.run(["cp", "-c", os.path.join(SRC, f), f"{self.lib}/{sub}/{self.tag}_{f}"], check=True)
        # Two byte-identical duplicates of Trip photos, planted in Misc.
        for i in (0, 1):
            subprocess.run(["cp", "-c", os.path.join(SRC, photos[i]), f"{self.lib}/Misc/{self.tag}_copy{i + 1}.JPG"], check=True)
        self.before_paths = self.tree()
        self.manifest = {}
        for rel in self.before_paths:
            self.manifest.setdefault(sha(os.path.join(self.lib, rel)), []).append(rel)
        self.db = os.path.join(self.state, "fileid.sqlite")

    def tree(self):
        return sorted(os.path.relpath(os.path.join(r, f), self.lib)
                      for r, _, fs in os.walk(self.lib) for f in fs if not f.startswith("."))

    def path_hashes(self):
        return {rel: sha(os.path.join(self.lib, rel)) for rel in self.tree()}

    def trashed(self):
        return [p for p in glob.glob(os.path.join(TRASH, "*")) if self.tag in os.path.basename(p)]

    def engine(self, binary=None):
        env = {"FILEID_TIMELINE_SPEECH": "0"}
        if RUST:
            binary = RUST
            os.makedirs(self.state + "/models", exist_ok=True)
            env.update(FILEID_DB=self.db, XDG_DATA_HOME=self.state + "/xdg", FILEID_MODELS_DIR=self.state + "/models")
        e = Engine(self.state, binary=binary, extra_env=env)
        assert e.db == self.db
        return e

    @property
    def undo_journal(self):
        return os.path.join(self.state, "xdg/FileID" if RUST else "", "restructure_undo.ndjson")

    def hidden(self):
        return [os.path.relpath(os.path.join(r, f), self.lib) for r, _, fs in os.walk(self.lib) for f in fs if f.startswith(".") and f != ".DS_Store"]

    def sql(self, query, args=()):
        con = sqlite3.connect(f"file:{self.db}?mode=ro", uri=True, timeout=10)
        try:
            return con.execute(query, args).fetchall()
        finally:
            con.close()

    def journal(self):
        try:
            return json.load(open(os.path.join(self.state, "trash-journal.json")))
        except (OSError, ValueError):
            return []

    def check(self, label, catalog_complete=True, allow_trash=False, layout=None, before_restart=False):
        """Assert the invariants. `layout=True` also demands every file back at its original path."""
        problems, notes = [], []
        on_disk = {}
        for rel in self.tree():
            on_disk.setdefault(sha(os.path.join(self.lib, rel)), []).append(rel)
        items = [i for b in self.journal() for i in b["items"]]
        journaled = {i["trashedPath"] for i in items if i["trashedPath"]}
        # A write-ahead entry (killed before its Trash location was saved) is found by inode.
        pending_inodes = set()
        for i in items:
            if not i["trashedPath"]:
                for row in i.get("rows", []):
                    ref = row.get("columns", {}).get("file_ref", {}).get("int") if row.get("table") == "files" else None
                    if ref is not None:
                        pending_inodes.add(ref & 0xFFFFFFFFFFFFFFFF)
        for p in self.trashed():
            if os.stat(p).st_ino in pending_inodes:
                journaled.add(p)
        in_trash = {}
        for p in self.trashed():
            in_trash.setdefault(sha(p), []).append(p)
        for h, rels in self.manifest.items():
            have = len(on_disk.get(h, []))
            trash = in_trash.get(h, [])
            if have == len(rels) and not trash:
                continue
            if have + len(trash) == len(rels):
                lost_journal = [t for t in trash if t not in journaled]
                if lost_journal:
                    problems.append(f"in Trash with NO journal entry (app cannot restore): {[os.path.basename(t) for t in lost_journal]}")
                elif not allow_trash:
                    notes.append(f"in Trash, journaled: {[os.path.basename(t) for t in trash]}")
                continue
            if have + len(trash) < len(rels):
                hidden = [h2 for h2 in self.hidden() if sha(os.path.join(self.lib, h2)) == h]
                where = f", stranded under hidden name {hidden}" if hidden else ""
                problems.append(f"LOST bytes of {rels} (disk {have}, trash {len(trash)}{where})")
            else:
                problems.append(f"DUPLICATED {rels}: disk {on_disk.get(h)} trash {trash}")
        unknown = [rels for h, rels in on_disk.items() if h not in self.manifest]
        if unknown:
            problems.append(f"unknown/corrupted bytes on disk: {unknown[:5]}")
        if layout and self.tree() != self.before_paths:
            problems.append(f"layout differs: missing {sorted(set(self.before_paths) - set(self.tree()))[:4]} extra {sorted(set(self.tree()) - set(self.before_paths))[:4]}")
        empties = [os.path.relpath(os.path.join(r, d), self.lib) for r, ds, _ in os.walk(self.lib)
                   for d in ds if not os.listdir(os.path.join(r, d))]
        if empties and layout:
            problems.append(f"empty folders left behind: {empties[:5]}")
        elif empties:
            notes.append(f"empty folders: {empties[:5]}")
        if os.path.exists(self.db):
            ok = self.sql("PRAGMA integrity_check")[0][0]
            if ok != "ok":
                problems.append(f"integrity_check: {ok}")
            rows = self.sql("SELECT id, path_text FROM files WHERE path_text LIKE ?", (self.lib + "/%",))
            ghosts = [p for _, p in rows if not os.path.exists(p)]
            if ghosts and before_restart:
                # The engine repairs rows of journaled Trash items when it next starts; the
                # post-restart check below must then find none.
                trashed_names = {os.path.basename(t) for t in self.trashed()}
                pending_moves = set()
                try:
                    for raw in open(os.path.join(self.state, "path-moves.ndjson")):
                        intent = json.loads(raw).get("intent")
                        if intent:
                            pending_moves.add(intent["from"])
                except (OSError, ValueError):
                    pass
                # A restructure move logged as applied but not yet reflected in its row.
                try:
                    for raw in open(self.undo_journal):
                        entry = json.loads(raw)
                        for key in ("src", "dst", "source", "destination"):
                            if entry.get(key):
                                pending_moves.add(entry[key])
                except (OSError, ValueError):
                    pass
                ghosts = [g for g in ghosts if os.path.basename(g) not in trashed_names and g not in pending_moves]
            if ghosts:
                problems.append(f"{len(ghosts)} catalog rows for missing files: {[os.path.relpath(g, self.lib) for g in ghosts[:4]]}")
            if catalog_complete:
                cataloged = {p for _, p in rows}
                lost = [rel for rel in self.tree() if os.path.join(self.lib, rel) not in cataloged]
                if lost:
                    problems.append(f"{len(lost)} present files with no catalog row: {lost[:4]}")
        status = "PASS" if not problems else "FAIL"
        line = f"{status} {label}" + (" | " + "; ".join(problems) if problems else "") + (" (" + "; ".join(notes) + ")" if notes else "")
        print(line, flush=True)
        RESULTS.append((label, status, problems))
        return not problems

    def restore_trash_by_hand(self):
        """Last resort for files the engine could not restore: put tagged Trash items back by name."""
        moved = 0
        for p in self.trashed():
            base = os.path.basename(p)
            for rel in self.before_paths:
                if os.path.basename(rel) == base and not os.path.exists(os.path.join(self.lib, rel)):
                    os.makedirs(os.path.dirname(os.path.join(self.lib, rel)), exist_ok=True)
                    os.rename(p, os.path.join(self.lib, rel))
                    moved += 1
                    break
        return moved


def start(lab, binary=None):
    e = lab.engine(binary)
    e.wait(lambda p: kind(p) == "ready", 60)
    return e


def startup_errors(e, seconds=2):
    errors, end = [], time.time() + seconds
    while time.time() < end:
        try:
            p = e.events.get(timeout=0.2)
        except Exception:
            continue
        if kind(p) == "error" and inner(p).get("kind") not in ("deep_analyze_unavailable",):
            errors.append(inner(p))
    return errors


def restart(lab, label):
    e = start(lab)
    errs = startup_errors(e)
    crashed = lab.sql("SELECT count(*) FROM scan_sessions WHERE status='running'")[0][0] if os.path.exists(lab.db) else 0
    if errs or crashed:
        print(f"FAIL {label} engine restart | errors={errs} running_sessions={crashed}", flush=True)
        RESULTS.append((label + " restart", "FAIL", errs))
    return e


def kill(e):
    e.p.send_signal(signal.SIGKILL)
    e.p.wait(10)


def scan(e, lab, timeout=900, rescan=False):
    e.send("startScan", {"rootPath": lab.lib, **({"rescan": True} if rescan else {})})
    done = e.wait(lambda p: kind(p) == "scanComplete", timeout)
    return inner(done)


def bulk(e, cmd, body, timeout=120):
    e.send(cmd, body)
    r = e.wait(lambda p: kind(p) in ("bulkActionResult",) or (kind(p) == "error"), timeout)
    return inner(r)


def kill_when(e, predicate, timeout=600, delay=0.0):
    """Kill the engine the moment an event matches."""
    e.wait(predicate, timeout)
    if delay:
        time.sleep(delay)
    kill(e)


def kill_on_disk(e, condition, timeout=60, poll=0.0005):
    """Kill the engine as soon as a filesystem condition holds (tight poll)."""
    end = time.time() + timeout
    while time.time() < end:
        if condition():
            kill(e)
            return True
        if e.p.poll() is not None:
            return False
        time.sleep(poll)
    kill(e)
    return False


def dup_ids(lab):
    return [r[0] for r in lab.sql("SELECT id FROM files WHERE path_text LIKE ? ORDER BY id", (f"%/{lab.tag}_copy%",))]


# ---------------------------------------------------------------- experiments

def exp_scan_kill():
    for point in ("discovering", "tagging", "postScan"):
        lab = Lab("scan-" + point)
        e = start(lab)
        e.send("startScan", {"rootPath": lab.lib})
        if point == "tagging":
            kill_when(e, lambda p: kind(p) == "batchSummary" and inner(p)["processedTotal"] >= 20)
        else:
            kill_when(e, lambda p: kind(p) == "phaseChanged" and inner(p) == point)
        lab.check(f"1 scan SIGKILL at {point}: right after kill", catalog_complete=False)
        e = restart(lab, f"1 scan {point}")
        state = lab.sql("SELECT status FROM scan_sessions")
        lab.check(f"1 scan SIGKILL at {point}: after restart (sessions={state})", catalog_complete=False)
        v = scan(e, lab)
        lab.check(f"1 scan SIGKILL at {point}: rescan ({v['processedFiles']} processed, {v['failedFiles']} failed)")
        e.close()


def exp_trash_kill():
    # (a) the two duplicates, killed at a range of delays; (b) a 30-file batch killed mid-way.
    for delay in (0.0, 0.004, 0.012, 0.03):
        lab = Lab("trash-dups")
        e = start(lab); scan(e, lab)
        ids = dup_ids(lab)
        e.send("trashFiles", {"fileIDs": ids})
        time.sleep(delay); kill(e)
        lab.check(f"2 trash dups SIGKILL +{delay * 1000:.0f}ms: after kill", catalog_complete=False, allow_trash=True, before_restart=True)
        e = restart(lab, "2 trash dups")
        lab.check(f"2 trash dups +{delay * 1000:.0f}ms: after restart", catalog_complete=False, allow_trash=True)
        restore_everything(e, lab, f"2 trash dups +{delay * 1000:.0f}ms")
        e.close()
    lab = Lab("trash-batch")
    e = start(lab); scan(e, lab)
    ids = [r[0] for r in lab.sql("SELECT id FROM files WHERE path_text LIKE ? ORDER BY id", (lab.lib + "/Trip/%",))]
    e.send("trashFiles", {"fileIDs": ids})
    kill_on_disk(e, lambda: len(lab.trashed()) >= len(ids) // 2)
    lab.check(f"2 trash batch of {len(ids)} SIGKILL mid-way: after kill ({len(lab.trashed())} in Trash)", catalog_complete=False, allow_trash=True, before_restart=True)
    e = restart(lab, "2 trash batch")
    lab.check("2 trash batch: after restart", catalog_complete=False, allow_trash=True)
    restore_everything(e, lab, "2 trash batch")
    e.close()


def restore_everything(e, lab, label):
    """Restore every journaled batch, then rescan; report anything the app could not put back."""
    for batch in lab.journal():
        r = bulk(e, "restoreFromTrash", {"batchID": batch["id"]})
        if r.get("failed"):
            print(f"   restore {batch['id'][:8]}: {r.get('succeeded')} ok, {r.get('failed')} failed {[m.get('message') for m in r.get('messages', []) if not m.get('ok')][:3]}")
    stranded = lab.trashed()
    lab.check(f"{label}: after restoreFromTrash", catalog_complete=False)
    if stranded:
        print(f"   {len(stranded)} file(s) the app could not restore; putting them back by hand: {[os.path.basename(s) for s in stranded][:4]}")
        lab.restore_trash_by_hand()
    scan(e, lab)
    lab.check(f"{label}: after rescan", layout=True)


def plan(e, lab):
    e.send("planRestructure", {"libraryRoot": lab.lib, "supportsPagedPlans": False})
    p = e.wait(lambda p: kind(p) in ("restructurePlan", "error"), 300)
    assert kind(p) == "restructurePlan", p
    return inner(p).get("moves") or []


def journal_lines(lab):
    try:
        return sum(1 for _ in open(lab.undo_journal))
    except OSError:
        return 0


def wait_restructure(e, timeout=300):
    p = e.wait(lambda p: kind(p) in ("restructureApplyResult", "error"), timeout)
    return kind(p), inner(p)


def exp_restructure_kill():
    for fraction in (0.1, 0.5, 0.9):
        lab = Lab(f"restructure-{int(fraction * 100)}")
        e = start(lab); scan(e, lab)
        moves = plan(e, lab)
        e.send("applyRestructure", {"libraryRoot": lab.lib, "moves": moves, "useSymlinks": False})
        target = max(1, int(len(moves) * fraction))
        kill_on_disk(e, lambda: journal_lines(lab) >= target)
        moved = sum(1 for m in moves if os.path.exists(m["destination"]))
        lab.check(f"3 apply {len(moves)} moves SIGKILL at journal {journal_lines(lab)} ({moved} moved): after kill", catalog_complete=False, before_restart=True)
        e = restart(lab, "3 apply")
        lab.check("3 apply: after restart", catalog_complete=False)
        e.send("undoRestructure", {"libraryRoot": lab.lib})
        k, r = wait_restructure(e)
        lab.check(f"3 apply {int(fraction * 100)}%: undo after restart ({k} applied={r.get('applied')} failed={r.get('failed')})", catalog_complete=False, layout=True)
        scan(e, lab)
        lab.check(f"3 apply {int(fraction * 100)}%: rescan after undo", layout=True)
        e.close()
    # Kill DURING the undo, then undo again.
    lab = Lab("restructure-undo")
    e = start(lab); scan(e, lab)
    moves = plan(e, lab)
    e.send("applyRestructure", {"libraryRoot": lab.lib, "moves": moves, "useSymlinks": False})
    k, r = wait_restructure(e)
    e.send("undoRestructure", {"libraryRoot": lab.lib})
    home = lambda: sum(1 for m in moves if os.path.exists(m["source"]))
    kill_on_disk(e, lambda: home() >= len(moves) // 2)
    lab.check(f"3 undo SIGKILL mid-undo ({home()}/{len(moves)} back): after kill", catalog_complete=False, before_restart=True)
    e = restart(lab, "3 undo")
    lab.check("3 undo: after restart, before the retry (catalog follows the files)", catalog_complete=False)
    e.send("undoRestructure", {"libraryRoot": lab.lib})
    k, r = wait_restructure(e)
    lab.check(f"3 undo retried after restart ({k} applied={r.get('applied')} failed={r.get('failed')})", catalog_complete=False, layout=True)
    scan(e, lab)
    lab.check("3 undo retried: rescan", layout=True)
    e.close()


def tool(e, action, timeout=300, **kw):
    rid = str(uuid.uuid4())
    e.send("toolRequest", {"request": dict(requestID=rid, action=action, **kw)})
    return inner(e.wait(lambda p: kind(p) == "toolResponse" and inner(p).get("requestID") == rid, timeout))


def exp_rename_tools_kill():
    lab = Lab("rename")
    e = start(lab); scan(e, lab)
    rows = lab.sql("SELECT id, path_text FROM files WHERE path_text LIKE ? ORDER BY id LIMIT 12", (lab.lib + "/Trip/%",))
    renames = [{"fileID": i, "newName": f"{lab.tag}_renamed_{n}.JPG"} for n, (i, _) in enumerate(rows)]
    e.send("renameFiles", {"renames": renames})
    newpath = lambda n, p: os.path.join(os.path.dirname(p), f"{lab.tag}_renamed_{n}.JPG")
    done = lambda: sum(1 for n, (_, p) in enumerate(rows) if os.path.exists(newpath(n, p)))
    kill_on_disk(e, lambda: done() >= 5)
    lab.check(f"4 rename 12 SIGKILL after {done()} renamed: after kill", catalog_complete=False, before_restart=True)
    e = restart(lab, "4 rename")
    lab.check("4 rename: after restart (catalog follows the files)", catalog_complete=False)
    stale = [(i, p) for n, (i, p) in enumerate(rows) if os.path.exists(newpath(n, p))
             and lab.sql("SELECT path_text FROM files WHERE id=?", (i,))[0][0] != newpath(n, p)]
    if stale:
        print(f"   {len(stale)} renamed file(s) whose catalog row still names the old path: {[os.path.basename(p) for _, p in stale]}")
    back = [{"fileID": i, "newName": os.path.basename(p)} for n, (i, p) in enumerate(rows) if os.path.exists(newpath(n, p))]
    r = bulk(e, "renameFiles", {"renames": back})
    print(f"   undo rename: {r.get('succeeded')} ok, {r.get('failed')} failed {[m.get('message') for m in r.get('messages', []) if not m.get('ok')][:3]}")
    lab.check("4 rename: after undo (rename back) post-restart", catalog_complete=False, layout=not stale)
    scan(e, lab)
    lab.check("4 rename: rescan", layout=not stale)
    e.close()

    lab = Lab("tools")
    e = start(lab); scan(e, lab)
    ids = [r[0] for r in lab.sql("SELECT id FROM files WHERE path_text LIKE ? ORDER BY id LIMIT 8", (lab.lib + "/Trip/%",))]
    recipe = {"kind": "photo", "format": "jpeg", "maxDimension": 2400, "allowUpscale": False}
    pv = tool(e, "preview", fileIDs=ids, recipe=recipe, destination=lab.out)
    assert pv.get("status") == "ok", pv
    op = pv["operationID"]
    e.send("toolRequest", {"request": dict(requestID=str(uuid.uuid4()), action="execute", fileIDs=ids, recipe=recipe, destination=lab.out, operationID=op)})
    outs = lambda: [f for f in os.listdir(lab.out) if not f.startswith(".")]
    kill_on_disk(e, lambda: len(outs()) >= 3)
    left_before = sorted(os.listdir(lab.out))
    lab.check(f"4 tools execute 8 SIGKILL after {len(outs())} outputs: originals after kill")
    e = restart(lab, "4 tools")
    time.sleep(3)
    state = lab.sql("SELECT state FROM catalog_operations WHERE id=?", (op,))[0][0]
    un = tool(e, "undo", operationID=op, destination=lab.out)
    leftovers = sorted(os.listdir(lab.out))
    ghost_rows = lab.sql("SELECT path_text FROM files WHERE path_text LIKE ?", (lab.out + "/%",))
    print(f"   tools: out after kill={left_before} state={state} undo={un.get('status')} '{(un.get('message') or '')[:90]}' left={leftovers} rows={len(ghost_rows)}")
    ok = un.get("status") == "ok" and not [f for f in leftovers if not f.startswith(".")] and not ghost_rows
    print(("PASS" if ok else "FAIL") + " 4 tools: undo after restart removes every published export and its catalog row", flush=True)
    RESULTS.append(("4 tools undo", "PASS" if ok else "FAIL", leftovers))
    lab.check("4 tools: originals after undo", layout=True)
    e.close()


def exp_wipe_kill():
    for delay in (0.0, 0.002, 0.006, 0.02):
        lab = Lab("wipe")
        e = start(lab); scan(e, lab)
        n = lab.sql("SELECT count(*) FROM files")[0][0]
        e.send("wipeLibrary", {})
        time.sleep(delay); kill(e)
        e = restart(lab, "5 wipe")
        after = lab.sql("SELECT count(*) FROM files")[0][0]
        state = lab.sql("SELECT count(*) FROM catalog_vector_state")[0][0]
        print(f"   wipe +{delay * 1000:.0f}ms: files {n} -> {after}, vector_state rows {state}")
        lab.check(f"5 wipe SIGKILL +{delay * 1000:.0f}ms: restart (files {n}->{after})", catalog_complete=after > 0)
        v = scan(e, lab)
        lab.check(f"5 wipe SIGKILL +{delay * 1000:.0f}ms: rescan ({v['processedFiles']} processed)", layout=True)
        e.close()


def collect(e, needed, timeout=600):
    """Gather events until every predicate in `needed` (name -> predicate) has matched once."""
    got, errors, end = {}, [], time.time() + timeout
    while len(got) < len(needed) and time.time() < end:
        try:
            p = e.events.get(timeout=1)
        except Exception:
            if e.p.poll() is not None:
                raise RuntimeError(f"engine exited {e.p.returncode}")
            continue
        if kind(p) == "error":
            errors.append(inner(p))
        for name, pred in needed.items():
            if name not in got and pred(p):
                got[name] = inner(p)
                break
    return got, errors


def exp_concurrency():
    # (a) startScan twice without waiting.
    lab = Lab("conc-double")
    e = start(lab)
    e.send("startScan", {"rootPath": lab.lib}); e.send("startScan", {"rootPath": lab.lib})
    got, errors = collect(e, {"done": lambda p: kind(p) == "scanComplete"})
    time.sleep(2)
    extra = [p for p in list(e.events.queue) if kind(p) == "scanComplete"]
    print(f"   double startScan: errors={[x.get('kind') + ': ' + x.get('message', '')[:60] for x in errors]} extra scanComplete={len(extra)}")
    lab.check("6a startScan twice")
    e.close()

    # (b) scan + trash + rename + apply + chat + wipe interleaved, nothing awaited.
    lab = Lab("conc-storm")
    e = start(lab); scan(e, lab)
    moves = plan(e, lab)
    ids = dup_ids(lab)
    rows = lab.sql("SELECT id, path_text FROM files WHERE path_text LIKE ? ORDER BY id DESC LIMIT 3", (lab.lib + "/Misc/%",))
    e.send("startScan", {"rootPath": lab.lib, "rescan": True})
    e.send("trashFiles", {"fileIDs": ids})
    e.send("renameFiles", {"renames": [{"fileID": i, "newName": f"{lab.tag}_storm_{n}.JPG"} for n, (i, _) in enumerate(rows)]})
    e.send("applyRestructure", {"libraryRoot": lab.lib, "moves": moves, "useSymlinks": False})
    e.send("chatRequest", {"request": dict(requestID=str(uuid.uuid4()), conversationID="c", action="send", text="move my duplicate files to the trash")})
    e.send("wipeLibrary", {})
    got, errors = collect(e, {
        "scan": lambda p: kind(p) == "scanComplete",
        "trash": lambda p: kind(p) == "bulkActionResult" and inner(p)["action"].startswith("trashFiles"),
        "rename": lambda p: kind(p) == "bulkActionResult" and inner(p)["action"] == "renameFiles",
        "apply": lambda p: kind(p) == "restructureApplyResult",
        "chat": lambda p: kind(p) == "chatResponse" and inner(p).get("status") in ("completed", "error"),
        "wipe": lambda p: kind(p) == "libraryWiped"})
    summary = {k: ({x: v.get(x) for x in ("succeeded", "failed", "applied", "ok", "processedFiles", "status")}) for k, v in got.items()}
    print(f"   storm: {summary} errors={[x.get('kind') for x in errors]}")
    lab.check("6b storm: right after (Trash allowed)", catalog_complete=False, allow_trash=True)
    for batch in lab.journal():
        bulk(e, "restoreFromTrash", {"batchID": batch["id"]})
    scan(e, lab)
    e.send("undoRestructure", {"libraryRoot": lab.lib}); k, r = wait_restructure(e)
    renamed = [(i, p) for n, (i, p) in enumerate(rows)]
    for n, (i, p) in enumerate(rows):
        cur = lab.sql("SELECT id, path_text FROM files WHERE path_text LIKE ?", (f"%/{lab.tag}_storm_{n}.JPG",))
        if cur:
            bulk(e, "renameFiles", {"renames": [{"fileID": cur[0][0], "newName": os.path.basename(p)}]})
    scan(e, lab)
    lab.check(f"6b storm: after restore + undo ({r.get('applied')} back, {r.get('failed')} failed) + rename back + rescan", layout=True)
    e.close()

    # (c) a file deleted and another renamed on disk while a scan runs.
    lab = Lab("conc-mutate")
    e = start(lab)
    victims = sorted(glob.glob(lab.lib + "/Misc/*_DSC*"))[:2]
    e.send("startScan", {"rootPath": lab.lib})
    e.wait(lambda p: kind(p) == "batchSummary", 300)
    os.remove(victims[0])
    os.rename(victims[1], victims[1].replace("_DSC", "_moved_DSC"))
    h0 = [h for h, rels in lab.manifest.items() if os.path.relpath(victims[0], lab.lib) in rels][0]
    lab.manifest[h0].remove(os.path.relpath(victims[0], lab.lib))
    if not lab.manifest[h0]:
        del lab.manifest[h0]
    e.wait(lambda p: kind(p) == "scanComplete", 600)
    lab.check("6c file deleted + renamed mid-scan (deleted one intentionally gone)")
    e.close()

    # (d) the root folder renamed away mid-scan (a drive unplugged), then back.
    lab = Lab("conc-unplug")
    e = start(lab); scan(e, lab)
    before = lab.sql("SELECT id, path_text, (SELECT count(*) FROM tags t WHERE t.file_id=f.id) FROM files f ORDER BY id")
    e.send("startScan", {"rootPath": lab.lib, "rescan": True})
    e.wait(lambda p: kind(p) == "batchSummary", 300)
    os.rename(lab.lib, lab.lib + "-unplugged")
    got, errors = collect(e, {"done": lambda p: kind(p) == "scanComplete"})
    mid = lab.sql("SELECT id, path_text, (SELECT count(*) FROM tags t WHERE t.file_id=f.id) FROM files f ORDER BY id")
    failed = lab.sql("SELECT count(*) FROM files WHERE failed=1")[0][0]
    os.rename(lab.lib + "-unplugged", lab.lib)
    lost = sorted({r[0] for r in before} - {r[0] for r in mid})
    print(f"   unplug: rows {len(before)} -> {len(mid)} (failed={failed}), tags {sum(r[2] for r in before)} -> {sum(r[2] for r in mid)}, scan={got.get('done')} errors={[x.get('kind') for x in errors]}")
    ok = not lost
    print(("PASS" if ok else "FAIL") + f" 6d root vanished mid-scan keeps every catalog row ({len(lost)} lost)", flush=True)
    RESULTS.append(("6d unplug rows", "PASS" if ok else "FAIL", lost))
    scan(e, lab)
    after = lab.sql("SELECT id FROM files ORDER BY id")
    lab.check(f"6d after the root returns + rescan (ids kept: {set(r[0] for r in before) <= set(r[0] for r in after)})", layout=True)
    e.close()


def exp_claim_kill():
    """Rust engine: trashFiles first renames each file to a hidden `.fileid-trash-<uuid>` claim.
    Kill the moment a claim exists, then check the file is still findable."""
    for attempt in range(3):
        lab = Lab(f"claim-{attempt}")
        e = start(lab); scan(e, lab)
        ids = [r[0] for r in lab.sql("SELECT id FROM files WHERE path_text LIKE ? ORDER BY id", (lab.lib + "/Trip/%",))]
        e.send("trashFiles", {"fileIDs": ids})
        caught = kill_on_disk(e, lambda: any(".fileid-trash-" in h for h in lab.hidden()), timeout=20, poll=0.0001)
        claims = [h for h in lab.hidden() if ".fileid-trash-" in h]
        lab.check(f"7 trash claim SIGKILL (claim seen={caught}, {len(claims)} hidden claims left): after kill", catalog_complete=False)
        e = restart(lab, "7 trash claim")
        time.sleep(1)
        claims = [h for h in lab.hidden() if ".fileid-trash-" in h]
        lab.check(f"7 trash claim: after restart ({len(claims)} hidden claims left)", catalog_complete=False)
        scan(e, lab)
        lab.check("7 trash claim: after rescan")
        e.close()


EXPERIMENTS = {"claim": exp_claim_kill, "conc": exp_concurrency, "scan": exp_scan_kill, "trash": exp_trash_kill, "restructure": exp_restructure_kill,
               "rename": exp_rename_tools_kill, "wipe": exp_wipe_kill}

if __name__ == "__main__":
    chosen = sys.argv[1:] or list(EXPERIMENTS)
    try:
        for name in chosen:
            print(f"== {name}", flush=True)
            EXPERIMENTS[name]()
    finally:
        # Never leave scratch clones in the owner's Trash, whatever an experiment did.
        swept = sum(lab.restore_trash_by_hand() for lab in LABS)
        stray = [p for lab in LABS for p in lab.trashed()]
        for p in stray:
            os.rename(p, os.path.join(ROOT, os.path.basename(p)))
        print(f"cleanup: {swept} put back into their labs, {len(stray)} moved out of the Trash to {ROOT}", flush=True)
    failed = [r for r in RESULTS if r[1] == "FAIL"]
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} checks passed")
    sys.exit(1 if failed else 0)
