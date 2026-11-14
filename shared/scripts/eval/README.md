# People and timeline evaluation

Scripts that score face clustering and video moments against hand labels. The labelled
data are private family photos and videos. They stay on the machine that made them and are
never committed: only these scripts and aggregate numbers (in `STATE.md`) live in the repo.

## Local layout (outside the repo and outside synced folders)

The scripts default to `~/FileIDDev/Eval` (set `FILEID_EVAL_ROOT` to use another folder).
Keep a copy of every labels file in a separate folder, such as `~/FileIDDev/Eval-backup`.

```
~/FileIDDev/Eval/
├── NOTES.md                    private: sources, conventions, anything with real names
├── home/                       isolated engine home with copies of the installed models
├── people/
│   ├── images/<event>/...      copies of the source photos (never the originals)
│   ├── labels.json             one record per face
│   └── runs/                   scratch catalogs and JSON reports
└── timeline/
    ├── videos/V01.mp4 ...      copies of the source videos
    ├── labels.json             labelled intervals and cuts per video
    ├── pools/                  the engine's 0.5 s frame pools (candidates mode)
    └── full-<name>/            scratch catalogs of end-to-end runs
```

`people/labels.json`:

```json
{"split": {"tune": ["event", "..."], "heldout": ["..."]},
 "faces": [{"id": "U0001", "event": "...", "image": "images/...", "sha256": "<content hash>",
            "width": 7008, "height": 4672, "bbox": [x, y, w, h], "identity": "P01"}]}
```

`bbox` is normalized, top-left origin, on the EXIF-upright image. `identity` is an anonymous id
(`P01`, `P02`, ...), `unsure` for a face whose identity cannot be judged, or `nonface` for a
detector hit that is not a live face (a printed poster, an ear). Never use real names.

`timeline/labels.json`:

```json
{"split": {...},
 "videos": [{"id": "V01", "event": "...", "sha256": "...", "duration": 28.0, "cuts": [3.35],
             "intervals": [{"start": 0, "end": 0.7, "label": "player throws ball",
                            "kind": "action", "fast": true}]}]}
```

`kind` is `action` (something happens), `scene` (a shot or subject segment worth a chapter) or
`quiet` (nothing significant happens; a negative). `fast` marks actions of 1.5 s or less.

Labels are split by event into tune and held-out halves so adjacent frames and bursts never
cross the split. Tune thresholds on the tune half only and report both.

## Running

Every engine run gets `FILEID_DATABASE_PATH`, `FILEID_DB` and `XDG_DATA_HOME` inside the work
folder, and `HOME`, `CFFIXED_USER_HOME`, `FILEID_HF_CACHE_ROOT` and `FILEID_MODELS_DIR` inside an
isolated home (`--home`, default `~/FileIDDev/Eval/home`). On first use the scripts copy the installed
Mac models into that home (`Library/Application Support/FileID/Models`, and the visual model for
`timeline_eval.py full` into `hf/models/<repo>`); the Rust engine's models go in `<home>/models`. Each
script compares the live FileID state folders and `~/Documents/huggingface` before and after and fails
if they changed. The scripts refuse work folders on protected volumes or inside the live state.

```bash
python3 shared/scripts/eval/people_eval.py --engine <FileIDEngine>
# the same, spelled out:
E=~/FileIDDev/Eval
python3 shared/scripts/eval/people_eval.py --engine <FileIDEngine> \
    --images $E/people/images --labels $E/people/labels.json --work $E/people/runs --home $E/home
# re-cluster the stored scan with an override:
python3 shared/scripts/eval/people_eval.py ... --name tight --env FILEID_FACE_PASS1_COS=0.70
# replay a growing library: cluster the earlier half of each event, then add the rest as new faces
python3 shared/scripts/eval/people_eval.py ... --name incremental --incremental
# cluster one half on its own (tune merge/split queue thresholds without held-out people mixed in)
python3 shared/scripts/eval/people_eval.py ... --name tune-merge60 --split tune --env FILEID_FACE_MERGE_SUGGEST_COS=0.60

python3 shared/scripts/eval/timeline_eval.py candidates --engine <FileIDEngine> \
    --videos $E/timeline/videos --labels $E/timeline/labels.json --work $E/timeline \
    --rule cut_cosine=0.85
python3 shared/scripts/eval/timeline_eval.py full --engine <FileIDEngine> \
    --videos $E/timeline/videos --labels $E/timeline/labels.json --work $E/timeline --model qwen3_vl_2b
```

With `--incremental`, `people_eval.py` holds back the later half of each event's photos (by image
path), clusters the rest, restores the held-back faces as new and clusters again. It reports, per split,
how many new faces were queued for review, how often the top suggestion is the face's labelled identity
(overall and per reason; a person's identity is the majority label of its earlier faces), the pairwise
numbers if every `suggested` item were accepted, and how many earlier faces changed group or new faces
were assigned (both 0 unless `FILEID_FACE_REVIEW_AUTO_ASSIGN=1`).

`people_eval.py` reports detection recall, pairwise precision/recall, B-cubed F1 (unassigned
and undetected faces count as singletons), a small-face subset (shorter side under 40 px on a
1536 px decode) and per-event numbers. The `--flavor rust` path parses the shared engine's
pixel boxes; it needs the YuNet and SFace models installed.

`timeline_eval.py candidates` is model-free: the engine's pool worker decodes the frames, a
photo scan of the pool gives CLIP vectors, and a port of `VideoSceneSelection` turns them into
scenes and activity windows, so thresholds can be swept in seconds. Keep the port's defaults
equal to the engine's. `full` runs the real moments job with an installed visual model; it
aborts if the engine starts a model download, and turns speech off because a bare engine
binary has no speech-recognition usage description. Both report interval recall at IoU 0.3,
cut timestamp error, proposal precision, the share of proposals inside quiet spans, and fast
actions that were never examined or never proposed.

`search_eval.py` scores text search strength on `<eval root>/search/labels.json` (photo list,
queries with `split`, `present`, `matches` and `ambiguous` photos). It copies a scanned catalog
(`--catalog`, CLIP image vectors stored), sends every query as the Mac Library does (hybrid, the
query words plus a client CLIP text vector from `--vector-engine`, default the engine under test)
and also semantic-only, and scores the files the reply calls good matches: macro precision and
recall over present queries, the share of present queries with a good match, and absent queries
answered with none. Pass the Swift engine as `--vector-engine` to compare engines with identical
client vectors; each engine still embeds its own prompt when CLIP text is installed in the home.

## Tags (`tag_eval.py`)

Scores the scan's stored tags against the 72 labelled subjects of the search set
(`search/labels.json`, the same 499 photos as People). A photo counts as tagged with a
subject when one of its tags is in the subject's accepted-word list inside the script, so
recall is a lower bound and precision an upper bound; ambiguous photos are left out.

```
tag_eval.py --engine <fileid-engine> --home <eval home> --name baseline          # scan + score
tag_eval.py --engine <fileid-engine> --home <eval home> --name wide --wide       # keep every RAM++ candidate >= 0.3
tag_eval.py --db <wide catalog> --home <eval home> --simulate --rules '{"refine": true, "max_tags": 10}'
```

The Rust engine on the Mac needs `--env ORT_DYLIB_PATH=/opt/homebrew/lib/libonnxruntime.dylib`
and an eval home whose `models/` holds the Rust layout (`ram_plus/`, `mobileclip/`, `clip_text/`,
`sface/`, `yunet/`). `--simulate` re-selects tags from a wide catalog the way the engine does,
so a rule can be tried in seconds; verify the engine afterwards with a default-mode scan.
