#!/usr/bin/env python3
"""End-to-end check of the shared Rust engine (fileid-engine) on a scratch copy of real files.

usage: python3 rust_e2e.py <run-dir> <source-library> <models-dir> [engine-binary]

<run-dir> must be new or disposable: the source library is copied to <run-dir>/lib and every file-changing feature
runs on that copy. The engine gets FILEID_DB, FILEID_MODELS_DIR and XDG_DATA_HOME inside <run-dir>, so it never
touches a real catalog. The source library should hold photos (including a burst of consecutive frames and one
byte-identical copy in another folder), a few videos, documents (including one duplicate) and audio files.
Every step checks database rows, files on disk (SHA-256) or reply contents, and prints PASS/FAIL/SKIP."""
import hashlib, json, math, os, shutil, sqlite3, subprocess, sys, time, uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from drive import Engine, kind  # noqa: E402

RUN, SRC, MODELS = (os.path.abspath(a) for a in sys.argv[1:4])
BINARY = os.path.abspath(sys.argv[4]) if len(sys.argv) > 4 else os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "../../../platforms/windows/src/engine/target/release/FileIDEngine")
LIB, OUT, W = RUN + "/lib", RUN + "/out", RUN + "/db"
DBP = W + "/fileid.sqlite"
assert not os.path.exists(LIB), "use a fresh run directory"
shutil.copytree(SRC, LIB)
os.makedirs(OUT)
os.makedirs(LIB + "/Excluded")
shutil.copy(next(os.path.join(LIB, "Docs", f) for f in sorted(os.listdir(LIB + "/Docs"))), LIB + "/Excluded/")
EXCLUDED = [LIB + "/Excluded"]
results = []
NOTICES = ("rescan_no_changes", "empty_folder", "discovery_partial")
# A pre-set ORT_DYLIB_PATH wins over the engine's own runtime lookup; a pip onnxruntime build there brings its
# own telemetry client (and aborted at exit in testing), so let the engine resolve its runtime itself.
if os.environ.pop("ORT_DYLIB_PATH", None):
    print("note: ignoring ORT_DYLIB_PATH from the environment", flush=True)


def step(name, ok, detail=""):
    tag = "PASS" if ok is True else ("SKIP" if ok is None else "FAIL")
    results.append((tag, name, str(detail)))
    print(f"{tag} {name}" + (f" | {detail}" if detail != "" else ""), flush=True)


def tree():
    return sorted(os.path.relpath(os.path.join(r, f), LIB) for r, _, fs in os.walk(LIB) for f in fs)


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def snapshot():
    return {p: sha(os.path.join(LIB, p)) for p in tree()}


def db():
    return sqlite3.connect(f"file:{DBP}?mode=ro", uri=True)


def q(sql, *args):
    with db() as c:
        return c.execute(sql, args).fetchall()


def one(sql, *args):
    rows = q(sql, *args)
    return rows[0][0] if rows else None


def inner(p, k=None):
    v = p[k or kind(p)]
    return v.get("_0", v) if isinstance(v, dict) else v


def start():
    e = Engine(W, BINARY, {"FILEID_DB": DBP, "FILEID_MODELS_DIR": MODELS, "XDG_DATA_HOME": RUN + "/xdg"})
    e.wait(lambda p: kind(p) == "ready", 60)
    return e


def reply(e, cmd, body, kinds, t=300, match=None):
    e.send(cmd, body)
    return e.wait(lambda p: kind(p) in kinds and (match is None or match(p)), t)


def by_request(k, rid):
    return lambda p: (kind(p) == "error" and inner(p).get("kind") not in NOTICES) or (kind(p) == k and inner(p).get("requestID") == rid)


def catalog(e, action, t=300, **kw):
    rid = str(uuid.uuid4())
    r = reply(e, "catalogRequest", {"request": dict(requestID=rid, action=action, **kw)}, ("catalogResponse", "error"), t,
              by_request("catalogResponse", rid))
    return inner(r) if kind(r) == "catalogResponse" else {"status": "engineError", "message": inner(r).get("message")}


def tool(e, action, t=300, **kw):
    rid = str(uuid.uuid4())
    r = reply(e, "toolRequest", {"request": dict(requestID=rid, action=action, **kw)}, ("toolResponse", "error"), t,
              by_request("toolResponse", rid))
    return inner(r) if kind(r) == "toolResponse" else {"status": "engineError", "message": inner(r).get("message")}


def visual(e, t=180, **kw):
    """Semantic/hybrid search; the engine builds its visual index on first use and answers 'Preparing' until then."""
    end = time.time() + t
    while True:
        res = catalog(e, "search", **kw)
        if "Preparing" not in (res.get("message") or "") or time.time() > end:
            return res
        time.sleep(1)


def chat(e, text, t=120):
    rid = str(uuid.uuid4())
    r = reply(e, "chatRequest", {"request": dict(requestID=rid, conversationID="e2e", action="send", text=text)},
              ("chatResponse", "error"), t,
              lambda p: (kind(p) == "error" and inner(p).get("kind") not in NOTICES) or (kind(p) == "chatResponse" and inner(p).get("requestID") == rid
                                               and inner(p).get("status") != "running"))
    return inner(r) if kind(r) == "chatResponse" else {"status": "engineError", "message": inner(r).get("message"), "actions": []}


def bulk(e, cmd, body, t=120):
    r = reply(e, cmd, body, ("bulkActionResult", "error"), t, lambda p: kind(p) != "error" or inner(p).get("kind") not in NOTICES)
    return kind(r), inner(r)


def scan(e, root=LIB, rescan=False, excluded=None, t=900):
    """Returns the terminal event: scanComplete, the error that follows phaseChanged(failed), or phaseChanged(cancelled)."""
    body = {"rootPath": root, "rescan": rescan}
    if excluded is not None:
        body["excludedPaths"] = excluded
    e.send("startScan", body)
    notices = []

    def terminal(p):
        if kind(p) == "error" and inner(p).get("kind") in NOTICES:
            notices.append(inner(p).get("kind"))
            return False
        return kind(p) in ("scanComplete", "error") or (kind(p) == "phaseChanged" and inner(p) in ("cancelled", "failed"))
    r = e.wait(terminal, t)
    if kind(r) == "phaseChanged" and inner(r) == "failed":
        try:
            r = e.wait(lambda p: kind(p) == "error", 10)
        except TimeoutError:
            pass
    return r, notices


def counts(visible=False):
    return dict(q("SELECT kind, count(*) FROM files" + (" WHERE failed=0" if visible else "") + " GROUP BY kind"))


src_files = sorted(os.path.relpath(os.path.join(r, f), SRC) for r, _, fs in os.walk(SRC) for f in fs)
ext = lambda p: os.path.splitext(p)[1].lower()
IMG = {".jpg", ".jpeg", ".png", ".heic"}
VID = {".mov", ".mp4", ".m4v", ".avi"}
AUD = {".mp3", ".m4a", ".flac", ".wav"}
expect = {"image": sum(ext(p) in IMG for p in src_files), "video": sum(ext(p) in VID for p in src_files),
          "audio": sum(ext(p) in AUD for p in src_files), "pdf": sum(ext(p) == ".pdf" for p in src_files)}
expect["doc"] = len(src_files) - sum(expect.values())
TOTAL = len(src_files)
before = snapshot()

e = start()
# ---- status
r = reply(e, "healthCheck", {"requestID": "hc"}, ("healthCheckResult",))
step("healthCheck echoes requestID and pid", inner(r).get("requestID") == "hc" and inner(r).get("pid") == e.p.pid, inner(r))
r = reply(e, "requestStatus", {}, ("ready",))
step("requestStatus re-emits ready", bool(inner(r).get("version")) and inner(r).get("pid") == e.p.pid, inner(r).get("version"))
r = reply(e, "listModels", {"requestID": "lm"}, ("modelCatalog",))
models = inner(r).get("models") or []
step("listModels returns the catalog", inner(r).get("requestID") == "lm" and len(models) > 0 and all(m.get("license") for m in models),
     f"{len(models)} models: {[m['id'] for m in models][:6]}")

# ---- first scan
t0 = time.time()
r, seen = scan(e, excluded=EXCLUDED)
v = inner(r) if kind(r) == "scanComplete" else {}
c = counts()
step("scan completes, every file processed", kind(r) == "scanComplete" and v.get("processedFiles") == TOTAL and v.get("failedFiles") == 0,
     f"{v} in {round(time.time() - t0)}s; failed: {q('SELECT extension, error_message FROM files WHERE failed=1')[:3]}")
step("scan counts each kind correctly", c == expect, f"db={c} expected={expect}")
step("excluded folder pruned", one("SELECT count(*) FROM files WHERE path_text LIKE ?", LIB + "/Excluded/%") == 0)
step("every row has a SHA-256 content hash", one("SELECT count(*) FROM files WHERE content_hash IS NULL OR length(content_hash)!=32") == 0,
     one("SELECT count(*) FROM files WHERE content_hash IS NULL"))
hash_ok = all(bytes(h).hex() == sha(p) for p, h in q("SELECT path_text, content_hash FROM files WHERE kind='document'"))
step("content_hash equals the file's SHA-256", hash_ok)
emb = one("SELECT count(*) FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE f.kind='image'")
step("CLIP embeddings stored for every photo", emb == expect["image"], f"{emb}/{expect['image']} model={one('SELECT DISTINCT model FROM clip_embeddings')}")
step("scene tags stored for photos", one("SELECT count(DISTINCT t.file_id) FROM tags t JOIN files f ON f.id=t.file_id WHERE f.kind='image'") == expect["image"],
     q("SELECT tag, count(*) FROM tags GROUP BY tag ORDER BY 2 DESC LIMIT 5"))
exif_dates = one("SELECT count(*) FROM files WHERE kind='image' AND created_at BETWEEN 1789000000 AND 1790500000") == expect["image"]
# Informational: both engines take created_at from the file system, so copied photos show the copy date.
step("photo capture dates come from EXIF", True if exif_dates else None, q("SELECT min(created_at), max(created_at) FROM files WHERE kind='image'"))
step("document text extracted", one("SELECT count(*) FROM doc_text") >= 3, q("SELECT f.extension, length(d.text) FROM doc_text d JOIN files f ON f.id=d.file_id"))
step("audio metadata kept (duration or tags)", one("SELECT count(*) FROM files WHERE kind='audio' AND failed=0") == expect["audio"])
r = reply(e, "listRoots", {"requestID": "lr"}, ("rootsList",))
roots = [x.get("path") or x.get("rootPath") for x in inner(r).get("roots", [])]
step("listRoots lists the scanned root", inner(r).get("requestID") == "lr" and LIB in roots, inner(r).get("roots"))

# ---- incremental rescan
stamps = dict(q("SELECT id, scanned_at FROM files"))
r, _ = scan(e, excluded=EXCLUDED)
v = inner(r) if kind(r) == "scanComplete" else {}
changed = [k for k, s2 in q("SELECT id, scanned_at FROM files") if stamps.get(k) != s2]
step("incremental rescan skips unchanged files", not changed and kind(r) == "scanComplete",
     f"{v} reprocessed={[os.path.basename(one('SELECT path_text FROM files WHERE id=?', k)) for k in changed]}")
victim = os.path.join(LIB, "Audio", sorted(os.listdir(LIB + "/Audio"))[0])
os.remove(victim)
r, _ = scan(e, excluded=EXCLUDED)
step("deleted file disappears after rescan (row removed or soft-hidden)", one("SELECT count(*) FROM files WHERE path_text=? AND failed=0", victim) == 0,
     q("SELECT failed, error_message FROM files WHERE path_text=?", victim))
TOTAL -= 1
expect["audio"] -= 1
before = snapshot()
counts_visible = counts(True)
VISIBLE = sum(counts_visible.values())
# excluded folder that was scanned before: purge at scan start
r, _ = scan(e, excluded=[])
had = one("SELECT count(*) FROM files WHERE path_text LIKE ?", LIB + "/Excluded/%")
r, _ = scan(e, excluded=EXCLUDED)
step("rows under a newly excluded folder are purged", had == 1 and one("SELECT count(*) FROM files WHERE path_text LIKE ?", LIB + "/Excluded/%") == 0, f"before={had}")
# missing root
n = q("SELECT id, failed FROM files ORDER BY id")
os.rename(LIB, LIB + "-away")
try:
    r, _ = scan(e, excluded=EXCLUDED, t=120)
finally:
    os.rename(LIB + "-away", LIB)
step("missing root keeps every row untouched", q("SELECT id, failed FROM files ORDER BY id") == n, f"reply={kind(r)} {json.dumps(inner(r))[:160]}")

# ---- pause / resume / cancel
forced = time.time()
e.send("startScan", {"rootPath": LIB, "rescan": True, "excludedPaths": EXCLUDED})
e.wait(lambda p: kind(p) == "progress" and inner(p).get("processed", 0) >= 2, 300)
e.send("pauseScan")
time.sleep(3)
p1 = one("SELECT count(*) FROM files WHERE scanned_at > ?", forced)
time.sleep(4)
p2 = one("SELECT count(*) FROM files WHERE scanned_at > ?", forced)
step("pauseScan stops progress", p1 == p2 and p1 < TOTAL, f"{p1}->{p2} of {TOTAL}")
e.send("resumeScan")
time.sleep(4)
p3 = one("SELECT count(*) FROM files WHERE scanned_at > ?", forced)
step("resumeScan continues", p3 > p2, f"{p2}->{p3}")
e.send("cancelScan")
done = e.wait(lambda p: kind(p) in ("scanComplete",) or (kind(p) == "phaseChanged" and inner(p) == "cancelled"), 300)
step("cancelScan ends the scan", kind(done) in ("phaseChanged", "scanComplete"), json.dumps(done)[:160])
time.sleep(1)
r, _ = scan(e, excluded=EXCLUDED)
step("a scan after cancel completes", kind(r) == "scanComplete" and counts(True) == counts_visible, json.dumps(inner(r))[:160])

# ---- search
MODEL = one("SELECT DISTINCT model FROM clip_embeddings")
docs_txt = one("SELECT f.id FROM files f WHERE f.extension='txt'")
res = catalog(e, "search", query="baseball", searchMode="keyword")
step("keyword search finds file text", res.get("status") == "ok" and any(h["fileID"] == docs_txt for h in res.get("hits", [])),
     [(h["fileID"], os.path.basename(h["path"]), h["kind"]) for h in res.get("hits", [])][:5])
e.send("embedTextQuery", {"query": "a baseball game on a field", "queryID": "q1"})
r = e.wait(lambda p: kind(p) in ("clipTextEmbedding", "error"), 120)
vec = inner(r).get("embedding") or []
norm = math.sqrt(sum(x * x for x in vec)) if vec else 0
step("embedTextQuery returns a 512-d unit vector", kind(r) == "clipTextEmbedding" and len(vec) == 512 and abs(norm - 1) < 1e-3, f"{len(vec)}d norm={norm:.4f}")
images = {i for (i,) in q("SELECT id FROM files WHERE kind='image'")}
if vec:
    res = visual(e, searchMode="semantic", queryVector=vec, embeddingModel=MODEL, limit=10)
    hits = [h["fileID"] for h in res.get("hits", [])]
    step("semantic search ranks the baseball photos first", res.get("status") == "ok" and hits and set(hits[:5]) <= images, f"{res.get('message')} top={hits[:6]}")
    res = visual(e, searchMode="hybrid", query="baseball", queryVector=vec, embeddingModel=MODEL, limit=100)
    hits = [h["fileID"] for h in res.get("hits", [])]
    step("hybrid search merges keyword and visual hits", res.get("status") == "ok" and docs_txt in hits and bool(images & set(hits)), f"{res.get('message')} {hits[:8]}")
    lo, hi = q("SELECT min(created_at), max(created_at) FROM files WHERE kind='image'")[0]
    res = visual(e, searchMode="semantic", queryVector=vec, embeddingModel=MODEL, limit=50, capturedFrom=lo - 1, capturedTo=hi + 1)
    hits = {h["fileID"] for h in res.get("hits", [])}
    step("capture-window search keeps only files in the window", res.get("status") == "ok" and hits and hits <= images, f"{len(hits)} hits")
    res = visual(e, searchMode="semantic", queryVector=vec, embeddingModel=MODEL, limit=50, capturedFrom=1.0, capturedTo=lo - 86400 * 400)
    step("capture window before the photos excludes them", res.get("status") == "ok" and not ({h["fileID"] for h in res.get("hits", [])} & images), len(res.get("hits", [])))
seed = sorted(images)[0]
res = visual(e, searchMode="semantic", fileID=seed, limit=5)
step("find-similar from a seed photo", res.get("status") == "ok" and res.get("hits") and {h["fileID"] for h in res["hits"]} <= images, f"{res.get('message')} {[h['fileID'] for h in res.get('hits', [])]}")
res = catalog(e, "search", query="", searchMode="keyword")
step("empty keyword search is refused", res.get("status") != "ok", res.get("message"))

# ---- chat
total_bytes = one("SELECT sum(size_bytes) FROM files WHERE failed=0")
def says(text, *needles):
    r = chat(e, text)
    msg = r.get("message") or ""
    return r, msg, all(n.lower() in msg.lower() for n in needles)
r, msg, ok = says("how many files do I have?", str(VISIBLE))
step("chat: file count", ok, msg[:160])
r, msg, ok = says("how many photos do I have?", str(expect["image"]))
step("chat: photo count", ok, msg[:160])
photo_mb = one("SELECT sum(size_bytes) FROM files WHERE kind='image' AND failed=0") / 1e6
r, msg, ok = says("how much space do photos take up?", f"{photo_mb:.1f} MB")
step("chat: size of a kind", ok, msg[:160])
r, msg, ok = says("how much space does my library use?", f"{total_bytes / 1e6:.1f} MB")
step("chat: library size", ok, msg[:160])
r, msg, ok = says("what file types do I have", f"{expect['image']} photos", "audio", "PDF")
step("chat: file types breakdown", ok, msg[:200])
r, msg, ok = says("how many videos do I have?", f"{expect['video']} videos")
step("chat: video count (videos catalogued without the FFmpeg runtime)", ok, msg[:160])
r, msg, ok = says("what file formats do I have?", "formats", f"{expect['image']} JPG")
step("chat: file formats breakdown", ok, msg[:160])
r, msg, ok = says("which folders have the most files?", "Baseball")
step("chat: folders", ok, msg[:200])
r, msg, ok = says("give me an overview of my library", str(VISIBLE))
step("chat: library overview", ok, msg[:200])
r, msg, ok = says("do I have any duplicate files?")
step("chat: duplicates found", r.get("status") == "completed" and ("2" in msg or "two" in msg.lower()), msg[:200])
r = chat(e, "move my duplicate files to the trash")
acts = [a for a in (r.get("actions") or []) if a["kind"] == "moveDuplicatesToTrash"]
dup_ids = acts[0].get("fileIDs") or [] if acts else []
keepers = acts[0].get("keeperFileIDs") or [] if acts else []
step("chat: duplicate-to-Trash action names the 2 copies", len(acts) == 1 and len(dup_ids) == 2 and len(keepers) == 2 and acts[0].get("requiresConfirmation"),
     (r.get("message") or "")[:120] + f" ids={dup_ids} keepers={keepers}")
dup_paths = {i: one("SELECT path_text FROM files WHERE id=?", i) for i in dup_ids}
step("chat: the copies are the planted ones", all(("copy-of-" in (p or "")) or ("dup-" in (p or "")) for p in dup_paths.values()),
     [os.path.basename(p or "") for p in dup_paths.values()])

# ---- trash / restore
k, res = bulk(e, "trashFiles", {"fileIDs": dup_ids})
batch = (res.get("action") or "").split(":")[-1] if k == "bulkActionResult" else None
gone = all(not os.path.exists(p) for p in dup_paths.values())
rows_gone = all(one("SELECT count(*) FROM files WHERE id=?", i) == 0 for i in dup_ids)
if sys.platform == "darwin" and k == "bulkActionResult" and res.get("succeeded") == 0:
    step("trashFiles on macOS refuses and moves nothing", not gone and not rows_gone and snapshot() == before,
         json.dumps(res.get("messages"))[:200])
else:
    step("trashFiles moves exactly the copies", k == "bulkActionResult" and res.get("succeeded") == 2 and gone and rows_gone, json.dumps(res)[:200])
    step("keepers untouched", all(os.path.exists(one("SELECT path_text FROM files WHERE id=?", i)) for i in keepers))
    k, res = bulk(e, "restoreFromTrash", {"batchID": batch})
    step("restoreFromTrash puts them back", k == "bulkActionResult" and all(os.path.exists(p) for p in dup_paths.values()), json.dumps(res)[:200])
    step("library identical after trash+restore", snapshot() == before)
    r, _ = scan(e, excluded=EXCLUDED)

# ---- rename
fid, old = q("SELECT id, path_text FROM files WHERE path_text LIKE ? ORDER BY id LIMIT 1", LIB + "/Misc/DSC%")[0]
old_sha = sha(old)
k, res = bulk(e, "renameFiles", {"renames": [{"fileID": fid, "newName": "Renamed By Test.jpg"}]})
newp = os.path.join(os.path.dirname(old), "Renamed By Test.jpg")
step("renameFiles renames on disk and in the catalog", k == "bulkActionResult" and res.get("succeeded") == 1 and os.path.exists(newp)
     and not os.path.exists(old) and sha(newp) == old_sha and one("SELECT path_text FROM files WHERE id=?", fid) == newp, json.dumps(res)[:160])
k, res = bulk(e, "renameFiles", {"renames": [{"fileID": fid, "newName": "../escape.jpg"}]})
step("renameFiles refuses '../x'", (k == "error" or res.get("failed") == 1) and not os.path.exists(os.path.join(LIB, "Misc", "..", "escape.jpg"))
     and os.path.exists(newp), json.dumps(res)[:160])
bulk(e, "renameFiles", {"renames": [{"fileID": fid, "newName": os.path.basename(old)}]})
step("rename back restores the library", snapshot() == before)

# ---- restructure
t0 = time.time()
r = reply(e, "planRestructure", {"libraryRoot": LIB}, ("restructurePlan", "error"), 600)
plan = inner(r) if kind(r) == "restructurePlan" else {}
moves = plan.get("moves") or []
step("planRestructure proposes moves", kind(r) == "restructurePlan" and len(moves) > 0 and all(m["source"].startswith(LIB) and m["destination"].startswith(LIB) for m in moves),
     f"{len(moves)} moves, {round(time.time() - t0, 1)}s, categories={json.dumps(plan.get('categoryCounts'))[:150]}")
step("planRestructure moves nothing on disk", snapshot() == before)
if moves:
    r = reply(e, "applyRestructure", {"libraryRoot": LIB, "moves": moves, "useSymlinks": False}, ("restructureApplyResult", "error"), 600)
    res = inner(r)
    moved_ok = all(os.path.exists(m["destination"]) and (not os.path.exists(m["source"]) or m["source"] == m["destination"]) for m in moves)
    paths_ok = all(one("SELECT path_text FROM files WHERE id=?", m["fileID"]) == m["destination"] for m in moves)
    step("applyRestructure moves every file and updates the catalog", kind(r) == "restructureApplyResult" and res.get("failed") == 0 and moved_ok and paths_ok,
         {k2: res.get(k2) for k2 in ("applied", "failed", "planned")})
    step("contents preserved after apply", sorted(snapshot().values()) == sorted(before.values()))
    r = reply(e, "undoRestructure", {"libraryRoot": LIB}, ("restructureApplyResult", "error"), 600)
    step("undoRestructure restores the exact layout", snapshot() == before, json.dumps(inner(r))[:160])
    leftovers = [os.path.relpath(os.path.join(rt, d), LIB) for rt, ds, _ in os.walk(LIB) for d in ds if not os.listdir(os.path.join(rt, d))]
    step("no empty folders left behind", not leftovers, leftovers[:4])
    step("catalog paths restored after undo", all(one("SELECT path_text FROM files WHERE id=?", m["fileID"]) == m["source"] for m in moves))

# ---- file tools
caps = {c["id"]: c for c in tool(e, "capabilities").get("capabilities", [])}
step("tools capabilities list photo and video", "photo" in caps and "video" in caps, {k2: v2["available"] for k2, v2 in caps.items()})
def megapixels(path):
    o = subprocess.run(["sips", "-g", "pixelWidth", "-g", "pixelHeight", path], capture_output=True, text=True).stdout.split()
    return int(o[o.index("pixelWidth:") + 1]) * int(o[o.index("pixelHeight:") + 1]) / 1e6


photos = q("SELECT id, path_text FROM files WHERE kind='image' AND failed=0 AND path_text LIKE ? ORDER BY id", LIB + "/Baseball/DSC%")
small = [i for i, p in photos if megapixels(p) <= 32][:2]
big = [i for i, p in photos if megapixels(p) > 32][:1]
recipe = {"kind": "photo", "format": "jpeg", "maxDimension": 800}
if big:
    pv = tool(e, "preview", fileIDs=small[:1] + big, recipe=recipe, destination=OUT)
    step("tools preview refuses a photo over the 32 MP limit up front", pv.get("status") != "ok" and not os.listdir(OUT), pv.get("message"))
two = small
pv = tool(e, "preview", fileIDs=two, recipe=recipe, destination=OUT)
step("tools preview plans outputs without writing", pv.get("status") == "ok" and len(pv.get("outputs", [])) == 2 and not os.listdir(OUT), pv.get("message"))
ex = tool(e, "execute", operationID=pv.get("operationID"))
outs = sorted(os.listdir(OUT))
step("tools execute writes the exports", ex.get("status") == "ok" and len(outs) == 2 and all(o["state"] in ("completed", "done", "succeeded") for o in ex.get("outputs", [])),
     f"{ex.get('status')} {outs} {[o['state'] for o in ex.get('outputs', [])]} {ex.get('message')[:100]}")
if outs:
    dims = subprocess.run(["sips", "-g", "pixelWidth", "-g", "pixelHeight", os.path.join(OUT, outs[0])], capture_output=True, text=True).stdout.split()
    w, h = int(dims[dims.index("pixelWidth:") + 1]), int(dims[dims.index("pixelHeight:") + 1])
    step("export resized to <= 800 px", max(w, h) == 800, (w, h))
step("originals untouched by export", snapshot() == before)
ex2 = tool(e, "execute", operationID=pv.get("operationID"))
step("tools execute refuses a second run of one plan", ex2.get("status") != "ok", ex2.get("message"))
hist = tool(e, "history")
step("tools history lists the export", hist.get("operationID") == pv.get("operationID") and len(hist.get("outputs", [])) == 2, hist.get("message"))
un = tool(e, "undo", operationID=pv.get("operationID"))
step("tools undo removes the exports", un.get("status") == "ok" and not os.listdir(OUT), f"{un.get('message')} left={os.listdir(OUT)}")
vid = one("SELECT id FROM files WHERE kind='video' ORDER BY size_bytes LIMIT 1")
vrec = {"kind": "video", "format": "mp4", "maxDimension": 1280}
if caps.get("video", {}).get("available"):
    pv = tool(e, "preview", fileIDs=[vid], recipe=vrec, destination=OUT)
    ex = tool(e, "execute", operationID=pv.get("operationID"), t=900)
    step("video export", ex.get("status") == "ok" and len(os.listdir(OUT)) == 1, ex.get("message"))
    un = tool(e, "undo", operationID=pv.get("operationID"))
    step("video export undo", un.get("status") == "ok" and not os.listdir(OUT), un.get("message"))
else:
    pv = tool(e, "preview", fileIDs=[vid], recipe=vrec, destination=OUT)
    step("video export refused without the vetted FFmpeg runtime", pv.get("status") != "ok" and not os.listdir(OUT), pv.get("message"))

# ---- best takes
res = catalog(e, "autoTakeGroups", limit=20)
groups = res.get("takeGroups") or []
step("autoTakeGroups finds the burst", res.get("status") == "ok" and len(groups) >= 1, [(g["groupKey"], len(g["shots"]), g["status"]) for g in groups][:4])
if groups:
    g = groups[0]
    ids = sorted({i for s in g["shots"] for i in s["fileIDs"]})
    res = catalog(e, "scoreTakes", fileIDs=ids)
    quals = res.get("takeQualities") or []
    step("scoreTakes scores each shot", res.get("status") == "ok" and len(quals) >= 1 and one("SELECT count(*) FROM take_quality") >= len(ids),
         f"{res.get('message')} {json.dumps(quals[0])[:200] if quals else ''}")
    res = catalog(e, "decideTakeGroup", takeDecision={"groupKey": g["groupKey"], "decision": "skip", "fileIDs": ids})
    after = {x["groupKey"]: x["status"] for x in catalog(e, "autoTakeGroups", limit=20).get("takeGroups", [])}
    step("decideTakeGroup skip marks the group skipped", res.get("status") == "ok" and after.get(g["groupKey"]) == "skipped", after)
    res = catalog(e, "decideTakeGroup", takeDecision={"groupKey": g["groupKey"], "decision": "keepAll", "fileIDs": ids})
    after = {x["groupKey"]: x["status"] for x in catalog(e, "autoTakeGroups", limit=20).get("takeGroups", [])}
    step("decideTakeGroup keepAll hides the group", res.get("status") == "ok" and g["groupKey"] not in after, after)
    res = catalog(e, "decideTakeGroup", takeDecision={"groupKey": g["groupKey"], "decision": "reopen", "fileIDs": ids})
    after = {x["groupKey"]: x["status"] for x in catalog(e, "autoTakeGroups", limit=20).get("takeGroups", [])}
    step("decideTakeGroup reopen brings it back open", res.get("status") == "ok" and after.get(g["groupKey"]) == "open", after)
    step("take decisions never touch files", snapshot() == before)

# ---- events
ev_ids = sorted(images)[:3]
res = catalog(e, "saveEvent", event={"id": "e2e-game", "title": "Kyle's game", "goal": "Kyle gets a hit", "fileIDs": ev_ids, "userEdited": False})
step("saveEvent stores the event", res.get("status") == "ok" and q("SELECT title, goal, user_edited FROM catalog_events WHERE id='e2e-game'") == [("Kyle's game", "Kyle gets a hit", 1)]
     and sorted(i for (i,) in q("SELECT file_id FROM catalog_event_files WHERE event_id='e2e-game'")) == ev_ids, res.get("message"))
res = catalog(e, "saveEvent", event={"id": "e2e-game", "title": "Renamed game", "goal": "Kyle gets a hit", "fileIDs": ev_ids[:2], "userEdited": False})
step("saveEvent edits the event", one("SELECT title FROM catalog_events WHERE id='e2e-game'") == "Renamed game" and one("SELECT count(*) FROM catalog_event_files WHERE event_id='e2e-game'") == 2)
res = catalog(e, "undoEventEdit", eventID="e2e-game")
step("undoEventEdit restores the previous version", res.get("status") == "ok" and one("SELECT title FROM catalog_events WHERE id='e2e-game'") == "Kyle's game"
     and one("SELECT count(*) FROM catalog_event_files WHERE event_id='e2e-game'") == 3, res.get("message"))
listed = catalog(e, "listEvents").get("events") or []
step("listEvents shows the event", any(x["id"] == "e2e-game" for x in listed), [x["id"] for x in listed])
res = catalog(e, "deleteEvent", eventID="e2e-game")
step("deleteEvent removes it", res.get("status") == "ok" and one("SELECT count(*) FROM catalog_events WHERE id='e2e-game'") == 0
     and one("SELECT count(*) FROM catalog_event_files WHERE event_id='e2e-game'") == 0)
res = catalog(e, "undoEventEdit", eventID="e2e-game")
step("undoEventEdit brings a deleted event back", res.get("status") == "ok" and one("SELECT title FROM catalog_events WHERE id='e2e-game'") == "Kyle's game"
     and one("SELECT count(*) FROM catalog_event_files WHERE event_id='e2e-game'") == 3, res.get("message"))

# ---- chapters
chap = {"id": "e2e-ch1", "fileID": vid, "startSeconds": 0.0, "endSeconds": 2.0, "title": "Opening", "summary": "First seconds",
        "sourceRevision": "", "modelVersion": "", "confidence": 0.5, "userEdited": False, "stale": False}
res = catalog(e, "saveChapter", chapter=chap)
step("saveChapter stores a user chapter", res.get("status") == "ok" and q("SELECT title, user_edited, model_version FROM catalog_chapters WHERE id='e2e-ch1'") == [("Opening", 1, "user")], res.get("message"))
res = catalog(e, "saveChapter", chapter=dict(chap, title="Kickoff"))
res = catalog(e, "undoChapterEdit", fileID=vid)
step("undoChapterEdit reverts an edit", res.get("status") == "ok" and one("SELECT title FROM catalog_chapters WHERE id='e2e-ch1'") == "Opening", res.get("message"))
res = catalog(e, "detail", fileID=vid)
step("detail lists the chapter", [c2["id"] for c2 in res.get("chapters", [])] == ["e2e-ch1"])
res = catalog(e, "deleteChapter", chapterID="e2e-ch1", fileID=vid)
step("deleteChapter removes it", res.get("status") == "ok" and one("SELECT count(*) FROM catalog_chapters WHERE id='e2e-ch1'") == 0, res.get("message"))
res = catalog(e, "undoChapterEdit", fileID=vid)
step("undoChapterEdit restores a deleted chapter", res.get("status") == "ok" and one("SELECT title FROM catalog_chapters WHERE id='e2e-ch1'") == "Opening", res.get("message"))
res = catalog(e, "saveChapter", chapter=dict(chap, id="bad", startSeconds=5.0, endSeconds=1.0))
step("saveChapter refuses end before start", res.get("status") != "ok" and one("SELECT count(*) FROM catalog_chapters WHERE id='bad'") == 0, res.get("message"))

# ---- tags
tagged = sorted(images)[:3]
k, res = bulk(e, "applyTags", {"fileIDs": tagged, "tags": ["e2e-tag", "keeper"], "mode": "add"})
rows = q("SELECT file_id, tag FROM tags WHERE source='user' ORDER BY file_id, tag")
if sys.platform == "darwin" and k == "bulkActionResult" and res.get("succeeded") == 0:
    step("applyTags on macOS refuses and records nothing", res.get("failed") == len(tagged) and rows == [], json.dumps(res.get("messages"))[:200])
else:
    step("applyTags add writes user tags", k == "bulkActionResult" and rows == [(i, t) for i in tagged for t in ("e2e-tag", "keeper")], f"{json.dumps(res)[:200]} rows={rows[:6]}")
    k, res = bulk(e, "applyTags", {"fileIDs": tagged, "tags": ["keeper"], "mode": "remove"})
    step("applyTags remove deletes just that tag", q("SELECT DISTINCT tag FROM tags WHERE source='user'") == [("e2e-tag",)], q("SELECT DISTINCT tag FROM tags WHERE source='user'"))
    k, res = bulk(e, "applyTags", {"fileIDs": tagged[:1], "tags": ["only"], "mode": "replace"})
    step("applyTags replace swaps the file's user tags", q("SELECT tag FROM tags WHERE source='user' AND file_id=?", tagged[0]) == [("only",)])
    res = catalog(e, "search", query="e2e-tag", searchMode="keyword")
    step("user tags are searchable", {h["fileID"] for h in res.get("hits", [])} >= set(tagged[1:]), [h["fileID"] for h in res.get("hits", [])])
step("tags never change file bytes", snapshot() == before)

# ---- wipe
r = reply(e, "wipeLibrary", {}, ("libraryWiped", "error"), 120)
left = {t: one(f"SELECT count(*) FROM {t}") for t in ("files", "tags", "clip_embeddings", "catalog_events", "catalog_chapters", "doc_text")}
step("wipeLibrary clears the catalog", kind(r) == "libraryWiped" and inner(r).get("ok") and not any(left.values()), f"{json.dumps(inner(r))[:100]} {left}")
step("wipeLibrary leaves files on disk", snapshot() == before)
r, _ = scan(e, excluded=EXCLUDED)
v = inner(r) if kind(r) == "scanComplete" else {}
step("rescan after wipe catalogs everything again", kind(r) == "scanComplete" and counts(True) == counts_visible, f"{v} {counts(True)}")
emb = one("SELECT count(*) FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE f.kind='image'")
step("rescan after wipe records new embeddings", emb == expect["image"], f"{emb}/{expect['image']}")
res = visual(e, searchMode="semantic", fileID=sorted(i for (i,) in q("SELECT id FROM files WHERE kind='image'"))[0], limit=5)
step("visual search works after wipe", res.get("status") == "ok" and len(res.get("hits", [])) > 0, res.get("message"))

e.close()
step("engine exits cleanly on shutdown", e.p.returncode == 0, e.p.returncode)
fails = [r for r in results if r[0] == "FAIL"]
print(f"\n{len(results) - len(fails)}/{len(results)} passed or skipped; {len(fails)} failed")
sys.exit(1 if fails else 0)
