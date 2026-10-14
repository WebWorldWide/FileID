# FileID — Known Issues

> Open defects across all platforms. Closed bugs are not kept here — once fixed,
> the detail lives in `git log` and (for non-obvious calls)
> [`DECISIONS.md`](DECISIONS.md). For *next work* with acceptance criteria see
> [`NEXT.md`](NEXT.md); for *what's working now* see [`STATE.md`](STATE.md).

Tiers: 🔴 Critical (broken core feature / data risk) · 🟠 High (significant UX
degradation) · 🟡 Medium (recoverable, latent, or large-library only) · 🟢 Low
(polish).

## 🔴 Critical

_None open._

## 🟠 High

_None open._

## 🟡 Medium

- [ ] **SFace identity clustering — Pass 1 is single-linkage** (cross-platform;
  Windows `pipeline/identity_clustering.rs` is the source of truth for the shared
  algorithm). Pass 1 is connected-components on a kNN graph, which can chain
  different people through bridge faces on very large libraries. The current
  bands (`pass1_cosine 0.66`, calibrated on hardware) exploit the gap between
  genuine clusters (~0.85+ mean cohesion) and chained blobs (~0.50) and fail
  *safe* toward over-split, so the symptom is extra singletons (mergeable in the
  UI), not silent identity merges. The structural fix is mutual-kNN /
  density-gated edges; a higher threshold alone would over-split genuine
  identities. Tuning needs a hand-labeled `G:\TrueNAS` subset — the calibration
  recipe is tracked in [`NEXT.md`](NEXT.md) under hardware UAT.

## 🟢 Low

_None open._
