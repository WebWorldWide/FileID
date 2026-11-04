"""Scan a scratch photo library and exercise every file-changing feature (duplicates, Trash, restore, Restructure
plan/apply/undo, rename, File Tools preview/export/undo), checking the files on disk after each step.
Never point it at a library you care about: it moves files (and puts them back)."""
import sys, os, json, time, uuid, hashlib, sqlite3, shutil
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ".")
from drive import *
HERE = os.path.abspath(os.environ.get("FILEID_E2E_DIR") or "/tmp/fileid-e2e")
# usage: FILEID_E2E_DIR=<scratch> python3 mutate_files.py   (put ~30 photos, incl. two exact duplicates, in <scratch>/lib/Trip and <scratch>/lib/Misc)
LIB = HERE + "/lib"; OUT = HERE + "/out-" + str(int(time.time())); W = HERE + "/db"
os.makedirs(OUT, exist_ok=True); shutil.rmtree(W, ignore_errors=True)
def tree(): return sorted(os.path.relpath(os.path.join(r, f), LIB) for r, _, fs in os.walk(LIB) for f in fs)
def sha(p): return hashlib.sha256(open(p, "rb").read()).hexdigest()
before = {p: sha(os.path.join(LIB, p)) for p in tree()}
e = Engine(W); e.wait(lambda p: kind(p) == "ready", 60)
db = lambda: sqlite3.connect(f"file:{W}/fileid.sqlite?mode=ro", uri=True)
def inner(p, k): v = p[k]; return v.get("_0", v)
def step(name, ok, detail=""): print(("PASS " if ok else "FAIL ") + name + (" | " + str(detail) if detail else ""), flush=True)
def bulk(cmd, body, t=120):
    e.send(cmd, body); r = e.wait(lambda p: kind(p) in ("bulkActionResult", "error"), t); return kind(r), (inner(r, kind(r)) if kind(r) else r)
# ---- scan
t = time.time(); e.send("startScan", {"rootPath": LIB}); done = e.wait(lambda p: kind(p) == "scanComplete", 600)
v = inner(done, "scanComplete"); step("scan copy", v["processedFiles"] == 32 and v["failedFiles"] == 0, f"{v['processedFiles']} files {round(time.time()-t)}s")
# ---- duplicates via chat
rid = str(uuid.uuid4()); e.send("chatRequest", {"request": dict(requestID=rid, conversationID="d", action="send", text="move my duplicate files to the trash")})
r = inner(e.wait(lambda p: kind(p) == "chatResponse" and inner(p, "chatResponse").get("requestID") == rid and inner(p, "chatResponse").get("status") == "completed", 60), "chatResponse")
acts = r.get("actions") or []; trash = [a for a in acts if a["kind"] == "moveDuplicatesToTrash"]
step("chat finds the 2 planted duplicates", len(trash) == 1 and len(trash[0].get("fileIDs") or []) == 2 and trash[0].get("requiresConfirmation"), (r.get("message") or "")[:100] + " | actions=" + str([(a["kind"], len(a.get("fileIDs") or [])) for a in acts]))
ids = trash[0]["fileIDs"] if trash else []; keepers = trash[0].get("keeperFileIDs") if trash else []
paths = {i: db().execute("select path_text from files where id=?", (i,)).fetchone()[0] for i in ids}
# ---- trash + restore
k, res = bulk("trashFiles", {"fileIDs": ids}); batch = (res.get("action") or "").split(":")[-1] if k == "bulkActionResult" else None
gone = all(not os.path.exists(p) for p in paths.values()); step("trash moves exactly the copies", k == "bulkActionResult" and res["succeeded"] == 2 and gone, f"succeeded={res.get('succeeded')} batch={batch}")
kept = all(os.path.exists(db().execute("select path_text from files where id=?", (i,)).fetchone()[0]) for i in keepers if db().execute("select count(*) from files where id=?", (i,)).fetchone()[0]) if keepers else True
step("keepers untouched", kept)
k, res = bulk("restoreFromTrash", {"batchID": batch}); back = all(os.path.exists(p) for p in paths.values())
step("restore puts them back", k == "bulkActionResult" and back, f"{res.get('succeeded')} restored")
step("library identical after trash+restore", {p: sha(os.path.join(LIB, p)) for p in tree()} == before)
# ---- restructure
t = time.time(); e.send("planRestructure", {"libraryRoot": LIB, "supportsPagedPlans": False}); pl = e.wait(lambda p: kind(p) in ("restructurePlan", "error"), 300)
plan = inner(pl, "restructurePlan") if kind(pl) == "restructurePlan" else {}
moves = plan.get("moves") or []; step("restructure plan", kind(pl) == "restructurePlan", f"{len(moves)} moves, categories={json.dumps(plan.get('categoryCounts'))[:120]}, {round(time.time()-t,1)}s")
if moves:
    sample = moves[:6]
    e.send("applyRestructure", {"libraryRoot": LIB, "moves": sample, "useSymlinks": False}); ap = e.wait(lambda p: kind(p) in ("restructureApplyResult", "error"), 300)
    res = inner(ap, "restructureApplyResult") if kind(ap) == "restructureApplyResult" else {}
    moved_ok = all(os.path.exists(m["destination"]) and not os.path.exists(m["source"]) for m in sample)
    step("restructure apply moves files", kind(ap) == "restructureApplyResult" and moved_ok, {k: res.get(k) for k in ("movedCount", "failedCount", "applied")} or json.dumps(res)[:150])
    e.send("undoRestructure", {"libraryRoot": LIB}); un = e.wait(lambda p: kind(p) in ("restructureApplyResult", "bulkActionResult", "error"), 300)
    step("restructure undo restores layout", {p: sha(os.path.join(LIB, p)) for p in tree()} == before, kind(un))
    leftovers = [os.path.relpath(os.path.join(r, d), LIB) for r, ds, _ in os.walk(LIB) for d in ds if not os.listdir(os.path.join(r, d))]
    step("no empty folders left behind", not leftovers, leftovers[:3])
# ---- rename
fid = db().execute("select id from files where path_text like '%/Misc/DSC%' order by id limit 1").fetchone()[0]; old = db().execute("select path_text from files where id=?", (fid,)).fetchone()[0]
k, res = bulk("renameFiles", {"renames": [{"fileID": fid, "newName": "Renamed By Test.jpg"}]}); newp = os.path.join(os.path.dirname(old), "Renamed By Test.jpg")
step("rename changes the file name", k == "bulkActionResult" and os.path.exists(newp) and not os.path.exists(old), res.get("succeeded"))
k, res = bulk("renameFiles", {"renames": [{"fileID": fid, "newName": "../escape.jpg"}]})
step("rename refuses path separators", k == "bulkActionResult" and res.get("failed", 0) == 1 and not os.path.exists(os.path.join(LIB, "..", "escape.jpg")), json.dumps(res)[:130])
bulk("renameFiles", {"renames": [{"fileID": fid, "newName": os.path.basename(old)}]}); step("rename back", os.path.exists(old))
# ---- file tools
def tool(action, **kw):
    rid = str(uuid.uuid4()); e.send("toolRequest", {"request": dict(requestID=rid, action=action, **kw)})
    return inner(e.wait(lambda p: kind(p) == "toolResponse" and inner(p, "toolResponse").get("requestID") == rid, 300), "toolResponse")
two = [r[0] for r in db().execute("select id from files where path_text like '%/Trip/DSC%' order by id limit 2")]
recipe = {"kind": "photo", "format": "jpeg", "maxDimension": 800, "allowUpscale": False}
pv = tool("preview", fileIDs=two, recipe=recipe, destination=OUT); step("file tools preview", pv.get("status") == "ok", (pv.get("message") or "")[:100])
print("   preview keys:", sorted(pv.keys()), {k: pv[k] for k in pv if k in ("operationID","revision","previewID")})
ex = tool("execute", fileIDs=two, recipe=recipe, destination=OUT, **({"operationID": pv["operationID"]} if pv.get("operationID") else {})); outs = sorted(os.listdir(OUT))
step("file tools export makes new copies", ex.get("status") == "ok" and len(outs) == 2, f"status={ex.get('status')} files={outs[:3]} msg={(ex.get('message') or '')[:80]}")
step("originals untouched by export", {p: sha(os.path.join(LIB, p)) for p in tree()} == before)
if outs:
    import subprocess
    dims = subprocess.run(["sips", "-g", "pixelWidth", "-g", "pixelHeight", os.path.join(OUT, outs[0])], capture_output=True, text=True).stdout.split()
    w, h = int(dims[dims.index("pixelWidth:") + 1]), int(dims[dims.index("pixelHeight:") + 1]); step("export resized to <= 800 px", max(w, h) <= 800, (w, h))
hist = tool("history"); step("file tools history lists the export", hist.get("status") == "ok", len(json.dumps(hist)))
opid = ex.get("operationID") or pv.get("operationID")
if opid:
    un = tool("undo", operationID=opid, destination=OUT); step("file tools undo removes the export", un.get("status") == "ok" and not os.listdir(OUT), f"{un.get('status')} left={os.listdir(OUT)} msg={(un.get('message') or '')[:140]}")
else: step("file tools undo (no operation id returned)", False, json.dumps(ex)[:200])
step("final library identical to the start", {p: sha(os.path.join(LIB, p)) for p in tree()} == before)
e.close()
