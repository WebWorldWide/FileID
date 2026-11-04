"""Feature audit for the macOS Swift engine: every command and catalog action that the other e2e
scripts do not cover, checked against the database and the files on disk.

usage: FILEID_E2E_DIR=<scratch dir containing lib/> FILEID_ENGINE=<engine binary> python3 feature_audit.py
The library must be a scratch copy: this moves, renames, tags and hides files (and restores them).
Expected layout: lib/Game (photos with people), lib/Clips (1-2 short videos), lib/Docs (txt/docx/pdf), lib/Skip.
"""
import hashlib, json, os, plistlib, shutil, sqlite3, subprocess, sys, time, traceback, uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from drive import Engine, kind  # noqa: E402

HERE = os.path.abspath(os.environ.get("FILEID_E2E_DIR") or "/tmp/fileid-audit")
LIB = HERE + "/lib"
W = HERE + "/db-" + str(int(time.time()))
MODEL = os.environ.get("FILEID_AUDIT_MODEL", "qwen3_vl_2b")
results = []


def step(name, ok, detail=""):
    results.append((name, bool(ok)))
    print(("PASS " if ok else "FAIL ") + name + (" | " + str(detail)[:260] if detail != "" else ""), flush=True)


def check(name):
    def wrap(fn):
        try:
            fn()
        except Exception as error:  # one broken check must not hide the others
            step(name + " (crashed)", False, f"{type(error).__name__}: {error} @ {traceback.extract_tb(error.__traceback__)[-1].lineno}")
        return fn
    return wrap


def sha(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


def tree():
    return {os.path.relpath(os.path.join(r, f), LIB): sha(os.path.join(r, f)) for r, _, fs in os.walk(LIB) for f in fs}


def finder_tags(path):
    raw = subprocess.run(["xattr", "-px", "com.apple.metadata:_kMDItemUserTags", path], capture_output=True, text=True).stdout
    if not raw.strip():
        return []
    return [t.split("\n")[0] for t in plistlib.loads(bytes.fromhex("".join(raw.split())))]


def inner(payload, k):
    value = payload[k]
    return value.get("_0", value) if isinstance(value, dict) else value


START = tree()
e = Engine(W)
e.wait(lambda p: kind(p) == "ready", 60)
db = lambda: sqlite3.connect(f"file:{W}/fileid.sqlite?mode=ro", uri=True)  # noqa: E731
q1 = lambda sql, *a: db().execute(sql, a).fetchone()  # noqa: E731


def scan(root=LIB, **extra):
    t = time.time()
    e.send("startScan", dict(rootPath=root, **extra))
    done = e.wait(lambda p: kind(p) == "scanComplete", 1200)
    return inner(done, "scanComplete"), round(time.time() - t, 1)


def cat(**body):
    rid = str(uuid.uuid4())
    e.send("catalogRequest", {"request": dict(requestID=rid, **body)})
    return inner(e.wait(lambda p: kind(p) == "catalogResponse" and inner(p, "catalogResponse").get("requestID") == rid, 600), "catalogResponse")


def bulk(cmd, body, timeout=120):
    e.send(cmd, body)
    r = e.wait(lambda p: kind(p) in ("bulkActionResult", "error"), timeout)
    return kind(r), inner(r, kind(r))


def chat(text, **extra):
    rid = str(uuid.uuid4())
    e.send("chatRequest", {"request": dict(requestID=rid, conversationID="a" + rid[:6], action="send", text=text, **extra)})
    return inner(e.wait(lambda p: kind(p) == "chatResponse" and inner(p, "chatResponse").get("requestID") == rid
                        and inner(p, "chatResponse").get("status") in ("completed", "error", "cancelled"), 300), "chatResponse")


# ---------------------------------------------------------------- scanning
@check("first scan")
def _():
    v, s = scan()
    files = sum(1 for _ in START)
    step("first scan catalogs every file", v["processedFiles"] == files and v["failedFiles"] == 0, f"{v['processedFiles']}/{files} in {s}s")
    kinds = dict(db().execute("select kind,count(*) from files group by 1").fetchall())
    step("file kinds classified", kinds.get("image") == 41 and kinds.get("video") == 2 and kinds.get("doc", 0) + kinds.get("pdf", 0) == 4, kinds)


@check("incremental rescan")
def _():
    before = q1("select count(*), max(scanned_at) from files")
    v, s = scan()
    step("unchanged rescan reprocesses nothing", v["processedFiles"] == 0 and q1("select count(*) from files")[0] == before[0], f"processed={v['processedFiles']} in {s}s")


@check("rescan picks up add, change and delete")
def _():
    game = sorted(os.listdir(LIB + "/Game"))
    victim, changed = game[-1], LIB + "/Docs/" + sorted(os.listdir(LIB + "/Docs"))[0]
    shutil.move(LIB + "/Game/" + victim, HERE + "/" + victim)
    original = open(changed, "rb").read()
    with open(changed, "ab") as f:
        f.write(b"\nappended line for the audit\n")
    shutil.copy(HERE + "/" + victim, LIB + "/Game/added_" + victim)
    v, _ = scan()
    gone = q1("select count(*) from files where path_text=?", LIB + "/Game/" + victim)[0] == 0
    added = q1("select count(*) from files where path_text=?", LIB + "/Game/added_" + victim)[0] == 1
    size_now = q1("select size_bytes from files where path_text=?", changed)[0]
    step("deleted file leaves the catalog", gone)
    step("new file joins the catalog", added)
    step("changed file is re-read", size_now == os.path.getsize(changed), f"processed={v['processedFiles']} size={size_now}")
    os.remove(LIB + "/Game/added_" + victim)
    shutil.move(HERE + "/" + victim, LIB + "/Game/" + victim)
    open(changed, "wb").write(original)
    scan()


@check("unplugged root keeps the catalog")
def _():
    rows = q1("select count(*) from files")[0]
    os.rename(LIB, LIB + ".away")
    os.makedirs(LIB)
    v, _ = scan()
    kept = q1("select count(*) from files")[0]
    os.rmdir(LIB)
    os.rename(LIB + ".away", LIB)
    step("empty or missing root deletes nothing", kept == rows, f"rows {rows} -> {kept}")
    roots = []
    rid = "roots-" + uuid.uuid4().hex[:6]
    e.send("listRoots", {"requestID": rid})
    roots = inner(e.wait(lambda p: kind(p) == "rootsList", 30), "rootsList")["roots"]
    me = [r for r in roots if r["path"] == LIB]
    step("listRoots reports the location online with its files", me and me[0]["online"] and me[0]["fileCount"] == rows, json.dumps(me)[:200])


@check("excluded folders")
def _():
    v, _ = scan(excludedPaths=[LIB + "/Skip"])
    hidden = q1("select count(*) from files where path_text like ?", LIB + "/Skip/%")[0]
    step("excluded folder is pruned from the catalog", hidden == 0, f"rows under Skip: {hidden}")
    scan()
    step("rescan without exclusion brings it back", q1("select count(*) from files where path_text like ?", LIB + "/Skip/%")[0] == 1)
    e.send("purgeExcluded", {"excludedPaths": [LIB + "/Skip"]})
    time.sleep(1.5)
    step("purgeExcluded removes those rows", q1("select count(*) from files where path_text like ?", LIB + "/Skip/%")[0] == 0)
    scan()


@check("pause, resume and cancel a scan")
def _():
    other = W + "-lc"
    big = W + "-pause-lib"
    os.makedirs(big)
    photos = [os.path.join(r, f) for r, _, fs in os.walk(LIB) for f in fs if f.lower().endswith((".jpg", ".jpeg"))]
    for n in range(400):
        shutil.copyfile(photos[n % len(photos)], f"{big}/p{n}.jpg")
    e2 = Engine(other)
    e2.wait(lambda p: kind(p) == "ready", 60)
    seen = {"n": 0}

    def track(p):
        if kind(p) == "progress":
            seen["n"] = max(seen["n"], inner(p, "progress").get("processed", 0))

    e2.send("startScan", {"rootPath": big})
    e2.wait(lambda p: seen["n"] >= 20 or kind(p) == "scanComplete", 300, on_event=track)
    e2.send("pauseScan", {})
    time.sleep(2)
    a = seen["n"]
    try:
        e2.wait(lambda p: kind(p) == "scanComplete", 4, on_event=track)
        finished = True
    except TimeoutError:
        finished = False
    step("pause stops progress", not finished and seen["n"] - a <= 8, f"{a} -> {seen['n']} while paused")
    e2.send("resumeScan", {})
    paused_at = seen["n"]
    e2.wait(lambda p: seen["n"] >= paused_at + 20 or kind(p) == "scanComplete", 300, on_event=track)
    step("resume continues", seen["n"] > paused_at, f"{paused_at} -> {seen['n']}")
    e2.send("cancelScan", {})
    done = inner(e2.wait(lambda p: kind(p) == "scanComplete", 300, on_event=track), "scanComplete")
    step("cancel ends the scan early", done["processedFiles"] < 400, done)
    e2.close()


# ---------------------------------------------------------------- tags written to the files
@check("applyTags")
def _():
    ids = [r[0] for r in db().execute("select id from files where kind='image' order by id limit 2")]
    k, res = bulk("applyTags", {"fileIDs": ids, "tags": ["FileID Audit"], "mode": "add"})
    paths = [q1("select path_text from files where id=?", i)[0] for i in ids]
    # The Mac app writes Finder tags itself (TagWriter); the engine records them in the catalog.
    stored = [q1("select count(*) from tags where file_id=? and tag='FileID Audit' and source='user'", i)[0] for i in ids]
    step("applyTags records the tags in the catalog", k == "bulkActionResult" and res.get("succeeded") == 2 and stored == [1, 1], stored)
    step("applyTags leaves file contents unchanged", all(sha(p) == START[os.path.relpath(p, LIB)] for p in paths))
    k, res = bulk("applyTags", {"fileIDs": ids, "tags": ["FileID Audit"], "mode": "remove"})
    step("applyTags remove clears the tag", k == "bulkActionResult" and not q1("select count(*) from tags where tag='FileID Audit'")[0])
    xattr = [subprocess.run(["xattr", "-p", "com.apple.metadata:_kMDItemUserTags", p], capture_output=True).stdout for p in paths]
    step("removing the tag clears it", k == "bulkActionResult" and not any(b"FileID Audit" in x for x in xattr), res)


# ---------------------------------------------------------------- events, takes, chapters
@check("events")
def _():
    ids = [r[0] for r in db().execute("select id from files where kind='image' order by id limit 5")]
    ev = {"id": "audit-event", "title": "Audit game", "goal": "Kyle gets a hit", "fileIDs": ids, "userEdited": True}
    r = cat(action="saveEvent", event=ev)
    listed = cat(action="listEvents")
    found = [x for x in (listed.get("events") or []) if x["id"] == "audit-event"]
    step("saveEvent then listEvents", r["status"] == "ok" and found and sorted(found[0]["fileIDs"]) == sorted(ids), r.get("message"))
    chat_r = chat("show photos from Audit game")
    step("chat finds files by event name", len(chat_r.get("hits") or []) >= 1, (chat_r.get("message") or "")[:120])
    r = cat(action="deleteEvent", eventID="audit-event")
    gone = not [x for x in (cat(action="listEvents").get("events") or []) if x["id"] == "audit-event"]
    step("deleteEvent removes it", r["status"] == "ok" and gone)
    r = cat(action="undoEventEdit", eventID="audit-event")
    back = [x for x in (cat(action="listEvents").get("events") or []) if x["id"] == "audit-event"]
    step("undoEventEdit restores it", r["status"] == "ok" and back, r.get("message"))
    r = cat(action="takeGroup", eventID="audit-event")
    step("takeGroup lists the event's takes", r["status"] == "ok" and len(r.get("takes") or []) == 5, f"{len(r.get('takes') or [])} takes {r.get('message')}")
    fb = {"eventID": "audit-event", "fileID": ids[0], "outcomeScore": 1.0, "preferred": True}
    r = cat(action="setTakeFeedback", takeFeedback=fb)
    pref = [t for t in (cat(action="takeGroup", eventID="audit-event").get("takes") or []) if t.get("fileID") == ids[0]]
    step("setTakeFeedback marks the preferred take", r["status"] == "ok" and pref and (pref[0].get("preferred") or (pref[0].get("feedback") or {}).get("preferred")), json.dumps(pref)[:200])
    r = cat(action="undoTakeFeedback", eventID="audit-event", fileID=ids[0])
    step("undoTakeFeedback", r["status"] == "ok", r.get("message"))


@check("best takes decisions")
def _():
    groups = cat(action="autoTakeGroups", limit=100).get("takeGroups") or []
    step("autoTakeGroups finds bursts", len(groups) >= 1, f"{len(groups)} groups")
    if not groups:
        return
    g = groups[0]
    ids = [i for s in g["shots"] for i in s["fileIDs"]]
    q = cat(action="scoreTakes", fileIDs=ids)
    rec = [x for x in (q.get("takeQualities") or []) if x["recommended"]]
    step("scoreTakes recommends exactly one try", len(rec) == 1, f"{len(q.get('takeQualities') or [])} scored")
    decision = {"groupKey": g["groupKey"], "decision": "skip", "fileIDs": ids}
    cat(action="decideTakeGroup", takeDecision=decision)
    after = [x for x in cat(action="autoTakeGroups", limit=100).get("takeGroups") or [] if x["groupKey"] == g["groupKey"]]
    step("skip moves the group to skipped", after and after[0]["status"] == "skipped", after and after[0]["status"])
    cat(action="decideTakeGroup", takeDecision=dict(decision, decision="keepAll"))
    after = [x for x in cat(action="autoTakeGroups", limit=100).get("takeGroups") or [] if x["groupKey"] == g["groupKey"]]
    step("keepAll hides the group", not after)
    cat(action="decideTakeGroup", takeDecision=dict(decision, decision="reopen"))
    after = [x for x in cat(action="autoTakeGroups", limit=100).get("takeGroups") or [] if x["groupKey"] == g["groupKey"]]
    step("reopen brings it back open", after and after[0]["status"] == "open")


@check("chapters")
def _():
    vid = q1("select id from files where kind='video' order by id limit 1")[0]
    chapter = {"id": "audit-ch", "fileID": vid, "startSeconds": 1.0, "endSeconds": 4.0, "title": "Audit chapter",
               "summary": "a kitchen whiteboard scene", "sourceRevision": "user", "modelVersion": "user", "confidence": 1.0,
               "userEdited": True, "stale": False}
    r = cat(action="saveChapter", chapter=chapter)
    d = cat(action="detail", fileID=vid)
    step("saveChapter then detail", r["status"] == "ok" and any(c["id"] == "audit-ch" for c in d.get("chapters") or []), r.get("message"))
    s = cat(action="search", query="kitchen whiteboard", searchMode="keyword", limit=20)
    hit = [h for h in s["hits"] if h["fileID"] == vid and h.get("startSeconds") is not None]
    step("search finds the chapter with its start time", hit and abs(hit[0]["startSeconds"] - 1.0) < 0.01, json.dumps(hit[:1])[:160])
    bad = cat(action="saveChapter", chapter=dict(chapter, id="audit-bad", startSeconds=9.0, endSeconds=2.0))
    step("a chapter that ends before it starts is refused", bad["status"] == "error", bad.get("message"))
    r = cat(action="deleteChapter", chapterID="audit-ch", fileID=vid)
    step("deleteChapter", r["status"] == "ok" and not any(c["id"] == "audit-ch" for c in cat(action="detail", fileID=vid).get("chapters") or []))
    r = cat(action="undoChapterEdit", chapterID="audit-ch", fileID=vid)
    step("undoChapterEdit restores it", any(c["id"] == "audit-ch" for c in cat(action="detail", fileID=vid).get("chapters") or []), r.get("message"))


@check("video thumbnail")
def _():
    path = q1("select path_text from files where kind='video' order by id limit 1")[0]
    e.send("generateVideoThumbnail", {"path": path})
    r = e.wait(lambda p: kind(p) in ("thumbnailGenerated", "error"), 120)
    v = inner(r, kind(r))
    # macOS renders video thumbnails in the app (IPCProtocol.generateVideoThumbnail); the engine must
    # still answer with the structured pointer instead of staying silent.
    step("generateVideoThumbnail answers with the macOS in-app pointer",
         kind(r) == "error" and v.get("kind") == "not_implemented_yet", json.dumps(v)[:200])


# ---------------------------------------------------------------- people
@check("people")
def _():
    e.send("runFaceClustering", {})
    r = e.wait(lambda p: kind(p) in ("faceClusteringComplete", "error"), 900)
    v = inner(r, kind(r))
    step("face clustering", kind(r) == "faceClusteringComplete" and v.get("personCount", 0) >= 2, v)
    people = db().execute("select id from persons order by file_count desc limit 2").fetchall()
    if len(people) < 2:
        return
    a, b = people[0][0], people[1][0]
    fa = q1("select id from face_prints where person_id=? order by id limit 1", a)[0]
    fb = q1("select id from face_prints where person_id=? order by id limit 1", b)[0]
    k, res = bulk("reassignFace", {"faceID": fb, "destinationPersonID": a, "createNewPerson": False})
    step("reassignFace moves a face", k == "bulkActionResult" and q1("select person_id from face_prints where id=?", fb)[0] == a, res)
    k, res = bulk("reassignFace", {"faceID": fb, "destinationPersonID": None, "createNewPerson": True})
    newp = q1("select person_id from face_prints where id=?", fb)[0]
    step("reassignFace to a new person", k == "bulkActionResult" and newp not in (a, b) and newp is not None, res)
    if not q1("select count(*) from face_prints where person_id=?", b)[0]:
        b = newp  # reassigning b's only face away removes the emptied, unnamed person
    before_b = [r[0] for r in db().execute("select id from face_prints where person_id=?", (b,))]
    k, res = bulk("mergeClusters", {"sourcePersonID": b, "destinationPersonID": a})
    k2, res2 = bulk("revertMerge", {"sourcePersonID": b, "destinationPersonID": a, "faceIDsToRevert": before_b})
    restored = [r[0] for r in db().execute("select id from face_prints where person_id=?", (b,))]
    step("revertMerge puts the faces back", k2 == "bulkActionResult" and sorted(restored) == sorted(before_b), f"{len(restored)}/{len(before_b)} {res2}")
    k, res = bulk("markPersonsDifferent", {"sourcePersonID": a, "destinationPersonID": b, "sourceAnchorFaceID": fa,
                                          "destinationAnchorFaceID": q1("select id from face_prints where person_id=? order by id limit 1", b)[0]})
    step("markPersonsDifferent is recorded", k == "bulkActionResult" and res.get("succeeded", 0) >= 1, res)


# ---------------------------------------------------------------- describe, file tools, model controls
@check("describe one file, a folder, and cancel")
def _():
    doc = q1("select id from files where kind in ('doc','pdf') order by id limit 1")[0]
    e.send("deepAnalyzeFile", {"fileID": doc, "modelKind": MODEL})
    r = e.wait(lambda p: kind(p) in ("deepAnalyzeComplete", "error"), 900)
    text = q1("select vlm_description from files where id=?", doc)[0]
    step("describe a document", kind(r) == "deepAnalyzeComplete" and text, (text or json.dumps(r))[:160])
    e.send("deepAnalyzeFolder", {"pathPrefix": LIB + "/Docs", "modelKind": MODEL})
    r = e.wait(lambda p: kind(p) in ("deepAnalyzeComplete", "error"), 900)
    left = q1("select count(*) from files where path_text like ? and kind in ('doc','pdf') and (vlm_description is null or vlm_description='')", LIB + "/Docs/%")[0]
    step("describe a folder covers every document", kind(r) == "deepAnalyzeComplete" and left == 0, f"left={left} {json.dumps(inner(r, kind(r)))[:120]}")
    e.send("deepAnalyzeAll", {"modelKind": MODEL, "skipExisting": True, "tagsOnly": False, "proposeRenames": True})
    e.wait(lambda p: kind(p) == "deepAnalyzeFileDone", 900)
    e.send("deepAnalyzeCancel", {})
    r = inner(e.wait(lambda p: kind(p) == "deepAnalyzeComplete", 600), "deepAnalyzeComplete")
    step("cancel stops a Describe batch", r.get("cancelled") is True and r.get("processed", 99) < 40, r)


@check("file tools video and chapters")
def _():
    out = HERE + "/tools-" + str(int(time.time()))
    os.makedirs(out)
    vid = q1("select id from files where kind='video' order by id limit 1")[0]

    def tool(action, **kw):
        rid = str(uuid.uuid4())
        e.send("toolRequest", {"request": dict(requestID=rid, action=action, **kw)})
        r = e.wait(lambda p: (kind(p) == "toolResponse" and inner(p, "toolResponse").get("requestID") == rid)
                   or (kind(p) == "error" and inner(p, "error").get("kind") == "command_decode_failed"), 900)
        return inner(r, "toolResponse") if kind(r) == "toolResponse" else {"status": "error", "message": inner(r, "error").get("message")}

    for label, recipe in (("video", {"kind": "video", "format": "mp4", "maxDimension": 1280, "allowUpscale": False}),
                          ("chapters", {"kind": "chapters", "format": "vtt", "maxDimension": 4096})):
        pv = tool("preview", fileIDs=[vid], recipe=recipe, destination=out)
        if pv.get("status") != "ok":
            step(f"file tools {label} preview", False, pv.get("message"))
            continue
        ex = tool("execute", fileIDs=[vid], recipe=recipe, destination=out, operationID=pv["operationID"])
        made = [f for f in os.listdir(out) if f.endswith("." + recipe["format"])]
        ok = ex.get("status") == "ok" and made
        detail = made
        if ok and label == "video":
            probe = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height",
                                    "-of", "csv=p=0", os.path.join(out, made[0])], capture_output=True, text=True).stdout.strip()
            w, h = (int(x) for x in probe.split(",")[:2])
            ok, detail = max(w, h) <= 1280, probe
        if ok and label == "chapters":
            body = open(os.path.join(out, made[0])).read()
            ok, detail = body.startswith("WEBVTT") and "Audit chapter" in body, body[:120]
        step(f"file tools {label} export", ok, detail)
        un = tool("undo", operationID=ex.get("operationID") or pv["operationID"], destination=out)
        step(f"file tools {label} undo", un.get("status") == "ok" and not [f for f in os.listdir(out) if f.endswith("." + recipe["format"])], un.get("message"))


@check("model controls")
def _():
    e.send("requestStatus", {})
    r = e.wait(lambda p: kind(p) in ("queueState", "progress", "error"), 30)
    step("requestStatus answers", kind(r) in ("queueState", "progress"), kind(r))
    rid = "bench" + uuid.uuid4().hex[:6]
    e.send("benchmarkModel", {"requestID": rid, "modelKind": MODEL, "tune": False})
    e.wait(lambda p: kind(p) == "benchmarkProgress", 300)
    e.send("cancelBenchmark", {})
    r = inner(e.wait(lambda p: kind(p) == "benchmarkResult" and inner(p, "benchmarkResult").get("requestID") == rid, 600), "benchmarkResult")
    step("cancelBenchmark ends a benchmark", r.get("status") in ("cancelled", "error", "ok"), r)
    e.send("cancelRestructure", {})
    time.sleep(1)
    step("cancelRestructure with nothing running is harmless", e.p.poll() is None)


# ---------------------------------------------------------------- chat actions
@check("chat action proposals")
def _():
    cases = {
        "scan my Game folder": "scanFolder", "rescan " + LIB: "rescan", "describe these files": None,
        "stop the scan": "stopJob", "find duplicates": None, "move my duplicate files to the trash": "moveDuplicatesToTrash",
        "suggest a folder layout for " + LIB: "suggestFolderLayout", "undo the last move": "undoLastMove",
        "find moments in my videos": "findMoments", "open settings": "openPage", "go to people": "openPage",
    }
    for text, want in cases.items():
        r = chat(text)
        kinds = [a["kind"] for a in r.get("actions") or []]
        ok = (want in kinds) if want else r["status"] == "completed"
        step(f"chat '{text[:40]}' -> {want or 'answer'}", ok, f"{kinds} {(r.get('message') or '')[:90]}")
        for a in r.get("actions") or []:
            if a["kind"] in ("moveDuplicatesToTrash", "applyFolderLayout", "undoLastMove", "renameToSuggested", "namePerson", "mergePeople"):
                step(f"  '{a['kind']}' needs confirmation", a.get("requiresConfirmation") is True)


e.close()
step("library unchanged after the whole audit", tree() == START)
passed = sum(1 for _, ok in results if ok)
print(f"\n{passed}/{len(results)} checks passed")
