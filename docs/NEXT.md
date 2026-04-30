# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

---

## 1. Soak the V8.5 Restructure tab

Verify the Sankey, hover bus, drill-down, and floating apply bar work end-to-end on the user's real ~50K library before adding more features.

**Acceptance:**
- Open Restructure with a non-trivial library. Sankey renders with no clipping at column edges, no shadow bleed between adjacent nodes.
- Hover any source/destination/recommendation card/staysPut row → cross-highlight reaches the Sankey + cards in sync. Tooltip shows file count + source → destination.
- Tap "+ N more folders" / "+ N more buckets" → drill-down sheet lists every file from the long-tail (no empty panel).
- Floating apply bar pinned to the bottom; primary CTA disabled until something's selected; convert-to-real-moves confirmation works.
- DA hint banner appears when DA captioned <40% of analyzable files; running DA from the banner kicks off a full library pass.

## 2. Engine perf sweep

Audit hot paths in the engine that we haven't touched recently. `ScanCoordinator`, `JobQueue`, `IPCSink` have lingering "no async operations occur within 'await' expression" warnings — investigate whether those are real (suggesting the wrapped expression doesn't need actor hop) and tighten.

**Acceptance:**
- 0 strict-concurrency warnings in `swift build`.
- Sustained scan throughput on the user's test corpus stays at ≥140 files/s on M1 Pro / 14 workers.

## 3. v1.0 ship checklist

`docs/SHIP.md` is the master list. Outstanding gates: code signing + notarization, app icon, About panel, Sparkle update channel.

**Acceptance:**
- `.app` is signed + notarized + stapled.
- First launch from a clean machine works without engine crash, model-missing dialog, or sandbox prompt issues.

## 4. Ideas parking lot

- Drag-and-drop a Restructure proposal row to override its destination.
- Per-cluster "merge into existing person" affordance in People (currently only across-card drag works).
- Smart Albums backed by saved CLIP queries.
- Export Restructure proposals as a JSON manifest for off-app review.
