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

- [ ] **SFace identity clustering — bridge faces can chain two people** (cross-platform;
  Windows `pipeline/identity_clustering.rs` is the source of truth for the shared
  algorithm). Pass 1 joins faces along a nearest-neighbour graph. The shared Rust
  engine already uses mutual-kNN edges (k = 32, on by default since the 2026-08-08
  calibration); the Swift engine and the Mac-hosted Rust adapter use plain
  single linkage with k = 10. On the labelled People set (847 faces, 126
  identities) mutual-kNN at k = 32 gives the same clusters as single linkage,
  so that set cannot show chaining. Measured 2026-11-14: tighter mutual gates
  (top-10, top-5) cut synthetic bridges but cost recall (0.05 to 0.17); a
  density gate does not cut bridges at all. Nothing was changed. The bands fail
  safe toward over-split (extra singletons, mergeable in the UI). Settling it
  needs a labelled library of several thousand faces that actually chains, or
  hand-labelled bridge faces ([`NEXT.md`](NEXT.md), group E).

## 🟢 Low

_None open._
