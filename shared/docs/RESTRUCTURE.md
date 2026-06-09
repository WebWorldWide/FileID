# Butler-grade restructure — design (on-device, research-backed)

> Synthesized from a 5-angle deep-research pass (2026-05). Replaces the current
> flat rule cascade (`pipeline/restructure.rs::classify` — Person → GPS →
> Document → Photos/Year/Month → …), which ignores CLIP/tags/clusters and so
> feels bland + loose. Everything here runs **fully on-device** (no cloud) on the
> signals FileID already computes. Sources at the end.

## North star

A "butler": proposes a reorganization that feels like *you* organized it —
extending your existing folder conventions, grouping by meaning + people + time,
auto-filing only what it's sure of and asking about the rest, always previewable
and one-click reversible.

## 1. Architecture — cluster-then-name (the key decision)

Geometric clustering discovers groups from fused signals; a **local VLM only
names/justifies** them (the TnT-LLM / TopicGPT pattern). Never ask an LLM to
cluster tens of thousands of files — it doesn't scale on-device. Math finds
structure; the LLM labels it; a cheap classifier assigns the long tail.

```
per-file signals ─▶ feature fusion ─▶ cluster (density) ─▶ learn-your-style
                                                              assignment
                                                                  │
   ┌──────────────────────────────────────────────────────────────┘
   ▼
nearest EXISTING folder? ──yes──▶ extend it (high confidence)
   │no
   ▼
new group ─▶ VLM names + justifies ─▶ hierarchy (label-then-group)
   │
   ▼
confidence tiers ─▶ auto-file (sure) / review queue (medium) / leave (unsure)
   │
   ▼
Sankey + before/after tree ─▶ user approves ─▶ reversible move journal
```

## 2. Feature fusion

Per file, build one vector from the signals already in the DB:
- **Per-block L2-normalize** each embedding (CLIP image 512-d, BGE text 384-d) so each lives on the unit hypersphere → cosine is the natural metric.
- **Scale each block by `wᵢ / √dᵢ`** then concatenate, so a 512-d block doesn't drown a 1-d feature (equalizes each modality's variance contribution).
- Tags → sparse multi-hot, L2-normalized. Face-cluster id + file kind → categorical (Gower-style match/mismatch). Path → weak prior only.
- **EXIF time**: never raw epoch. Cyclical (sin/cos day-of-year + time-of-day) + log-compressed absolute axis. For photos, first do **time-gap event segmentation** (split at adaptive gaps), then sub-cluster each event by content — the classic, near-free albuming cascade.
- **Default weights** (expose as "organize by content / date / people" sliders): image 0.35, text 0.30, tags 0.10, time 0.10, face 0.10, path 0.05.

## 3. Clustering

- **v1 (no new deps):** reuse the engine's existing two-pass density clusterer (`pipeline/identity_clustering.rs`, already calibrated for faces) on fused file vectors. Connected-components core + margin-gated assignment + variance split = exactly the over-merge-resistant behavior we want; noise → `Unsorted`.
- **Upgrade path:** UMAP (`n_components=8, n_neighbors=30, min_dist=0.0, cosine`) → HDBSCAN (`min_cluster_size≈25, min_samples=10, eom, prediction_data=true`); noise label (-1) is a feature → Misc. Hierarchy from HDBSCAN's condensed tree; Ward agglomerative to split oversized clusters into tidy sub-folders; k-means+silhouette only as a "split into exactly N" tool. Rust: `petal-clustering` / `linfa` (needs a deps decision).
- Cache reduced vectors + use incremental `approximate_predict` so new files don't trigger a full recompute. Run heavy re-clustering as a deferred background job (charging/idle), like Apple Photos.

## 4. Learn-your-style (Dropbox "Smart Move" pattern — the personalization core)

- **Bootstrap free ground truth:** every existing folder = a labeled class; its current files = examples. No user labeling, no cloud.
- **Folder prototype = mean embedding of its contents** (Nearest-Class-Mean / Prototypical Networks). One cached vector per folder; assignment = nearest-centroid lookup; online-updatable on every accepted move.
- **Score `(file → folder)` from three signals:** `α·cos(file, folder_name_emb) + β·mean_cos(file, sibling_files) + γ·folder_prior` (prior = file-count × recency/frequency). The sibling term captures the user's actual habit independent of folder naming.
- **Assign to the nearest EXISTING folder** when it clears the bar; only propose a **new** folder when nothing does. Abstain (leave in place / "needs review") on low top-1−top-2 margin — abstaining beats misfiling for a personal-files app.
- **Mimic conventions:** infer case style, separators, date format/position, and target **depth** from the destination folder's siblings + the tree's depth distribution; render new names into that template. A flat-`~/Taxes2024` user must not get `Documents/Finance/Taxes/2024/Q1`.
- **Learn from corrections:** every accept/reject updates the folder centroid + thresholds; store rejected pairs as negatives + a small kNN "correction memory" of recent decisions. No retraining needed.
- For files fitting no folder, induce only a **small extension** of the tree (Chain-of-Layer: seed with the user's actual top-level folders, few-shot a sibling/child under the closest branch), with self-consistency (sample 3-5×, keep recurring names) to kill hallucinated `Misc/Stuff/New Folder`.

## 5. VLM naming (Qwen2.5-VL 7B, local, batch)

- Feed a **cluster profile, not raw items:** ~10-15 distinctive c-TF-IDF terms + 3-5 representatives chosen by centrality (medoid) + diversity (max-min). Distinctive terms are the single highest-impact input.
- For photo clusters, send 3-6 downscaled thumbnails + cheap grounding (EXIF date span, GPS→place, top objects/faces) — VLMs are weak at dates/places, so supply them.
- **Label-then-reason** single call: `{"name": "Beach Vacation", "reason": "…"}`. Constrained decoding (llama.cpp GBNF / JSON schema) to force a 2-4 word Title-Case name; temp 0; 2-3 seed examples (more than 3 degrades). Forbid generic names (Misc/Files/Images); validate length 3-50 chars.
- **Hierarchy = label-then-group:** name flat clusters, then a second LLM pass over *just the labels* groups them into 5-12 parents. Merge near-duplicate sibling labels via embedding cosine ≥ 0.5 + LLM confirm. Cache each name by a hash of its representative IDs (stable across runs).

## 6. Confidence-tiered autonomy + trust

- **Three calibrated bands:** auto-apply ≥ 0.95 · suggest/one-click 0.70-0.95 · ask/hold < 0.70. Bands must map to *measured* per-category accuracy, not raw scores.
- **Gate by action risk:** move-within-tree (reversible) < rename (breaks links) < delete (destructive). Deletes/dedupe → quarantine folder with retention, never hard-unlink.
- **Earned, never-assumed autonomy:** start suggest-only; offer to promote a *category* a tier only after a streak ("accepted 20/20 screenshot moves — auto-file these?").
- **Reversibility = command journal:** each move/rename stores its inverse; "Undo last run" reverses the whole batch as one macro (compensate on partial failure). Already aligns with the existing shortcut-then-real-move flow.
- **Trust mechanics:** dry-run preview is the default (Hazel-style per-item match/✗ + "why"); one-tap Yes/No/Skip cards (the "Skip" defers without a wrong answer); plain-language "Why filed here" (filename + tags + sibling-match + confidence); per-category accuracy track record; always-visible Pause / Preview / Undo.

## 7. Visualization — Sankey stays the hero, augmented

Sankey is purpose-built for "files flow source-folder → destination-category" (2 columns, occasionally 3); it's the right primary view. Pair it with a **before/after side-by-side tree** as the "what exactly happens to *this* folder" confirmation view. Treemap / sunburst encode hierarchy/proportion, not movement — at most a supplementary "destination composition" panel.

Make the Sankey world-class:
- **Aggregate to folder/category nodes — never one node per file** (thousands → tens of nodes). Bucket the long tail into an expandable `Other` per tier.
- **Barycentre node ordering** (Sugiyama median-of-neighbours, iterative sweeps) to cut crossings; within a node sort links by target slope + magnitude (deterministic so it doesn't reshuffle between runs).
- **Color links by DESTINATION category** (the question is "what ends up in Invoices?"); neutral/gray source nodes; **Okabe-Ito CVD-safe palette** capped at ~8 hues (brand gold/lavender/cyan/pink for chrome only — 4 hues, not CVD-validated). Link opacity 0.4-0.6, → 0.9 on hover.
- **Hover = end-to-end path highlight + fade**, tooltip with count / size / % of source / example filenames. Node labels always on; link detail on hover.
- **Drill-down:** click a category → expand `Other`/subcategories; click a source → filter the side-by-side tree to that folder; animate layout transitions (~300-500 ms).
- On-device: aggregated graph is tens of nodes → trivial 60 fps in **Win2D** (native, not WebView); barycentre layout is sub-ms; ship coordinates over the existing IPC. Mirrors the macOS Sankey reference.

## Phased implementation plan

Status as of 2026-05-30: **P1–P4 built + headless-verified on Windows; macOS mirror written
(unverified — needs a Mac build). On-hardware butler-quality verification + threshold tuning
pending.** See `STATE.md` / `NEXT.md`.

- **P1 — engine: semantic + learn-your-style classify. ✅ Done (Windows).** `pipeline/restructure_semantic.rs`: fuse signals → cluster (reuses `identity_clustering`) → folder-prototype assignment with confidence/abstain. Pure Rust, unit-tested; rule cascade is the fallback.
- **P2 — naming. ◑ Partial.** c-TF-IDF distinctive-term group naming is live (the always-on de-bland win). Live local-VLM naming (Qwen2.5-VL label-then-reason, constrained, cached) is **deferred to a background pass** — a per-call `llama-mtmd-cli` subprocess reloads the model, too slow for an interactive plan. The cluster-profile inputs (distinctive terms + representatives) are the drop-in.
- **P3 — confidence tiers. ✅ Done (Windows); ◑ corrections pending.** 3-band routing (auto/review/ask) from folder-match strength + top-1−top-2 margin + cohesion, plain-language reasons, selective apply (holds "ask" back). Bands are provisional cosine thresholds — calibrate to *measured* accuracy. Learn-from-corrections + earned-autonomy promotions + command-journal undo are the follow-on.
- **P4 — visualization. ◑ Partial.** Sankey gained the Okabe-Ito CVD-safe palette + an "Other" long-tail node (no silent drop); barycentre ordering + hover highlight + drill-down already existed. Win2D upgrade, before/after tree, and weight sliders remain.
- **macOS — ◑ Written, unverified.** `RestructureSemantic.swift` mirrors the engine; `proposeAll` + IPC carry confidence/reason; app-side UI wiring is documented in `platforms/apple/MACOS_BUTLER_NOTES.md`.

## Sources

Taxonomy/clustering: TnT-LLM (arXiv 2403.12173), UMAP docs, BERTopic tuning, Gower distance (PMC11654179), Temporal Event Clustering (ACM 1083317), Iterative Topic Taxonomy (arXiv 2510.15125). Learn-style: Dropbox Smart Move (dropbox.tech) + patent (USPTO 12072839), Prototypical Networks (arXiv 1703.05175), Chain-of-Layer (arXiv 2402.07386), LlamaFS, ai-file-sorter, the "tree LLM" trick. Confidence/trust: Apple Photos clustering (machinelearning.apple.com), Hazel rule preview, Google Files/Photos, Gmail sorting, command-pattern undo, trust-calibration (aiuxdesign.guide), agentic-AI autonomy (uxmatters). VLM naming: TopicGPT (arXiv 2311.01449), cluster-labeling study (arXiv 2511.02601), constrained outputs (Helicone), Qwen2.5-VL (qwen.ai). Viz: Sankey vs sunburst (CleverTap), barycentre crossing reduction (arXiv 1912.05339), optimal Sankey (Monash), d3-sankey + d3-sankey-diagram, Okabe-Ito palette, large bipartite aggregation (ResearchGate 328993345).
