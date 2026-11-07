# People and timeline evaluation

Scripts that score face clustering and video moments against hand labels. The labelled
data are private family photos and videos. They stay on the machine that made them and are
never committed: only these scripts and aggregate numbers (in `STATE.md`) live in the repo.

## Local layout (outside the repo and outside synced folders)

```
~/Library/Developer/FileIDEval/
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
folder, and each script compares the live FileID state folders before and after and fails if
they changed. The scripts refuse work folders on protected volumes or inside the live state.

```bash
E=~/Library/Developer/FileIDEval
python3 shared/scripts/eval/people_eval.py --engine <FileIDEngine> \
    --images $E/people/images --labels $E/people/labels.json --work $E/people/runs
# re-cluster the stored scan with an override:
python3 shared/scripts/eval/people_eval.py ... --name tight --env FILEID_FACE_PASS1_COS=0.70
# replay a growing library: cluster the earlier half of each event, then add the rest as new faces
python3 shared/scripts/eval/people_eval.py ... --name incremental --incremental

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
