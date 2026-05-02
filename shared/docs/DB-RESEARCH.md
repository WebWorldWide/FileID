# Database Research — Should FileID swap SQLite?

> Requested 2026-04-28 by user after observing slow merge UI on a 1.84M-pair Suggested Merges sheet.
> User-confirmed: pain is in (a) Suggested Merges sheet rendering / merging and (b) face clustering wall time.
> Both are addressed by M2-M4 work. This doc evaluates whether a database swap would help further.

## TL;DR

**No. Don't swap SQLite.** The bottleneck has been ANE throughput (face embedding, Vision passes, MLX inference) and clustering algorithm choice — not the database. SQLite WAL with GRDB.swift handles FileID's workload comfortably:

- Insert throughput ceiling: ~30-40k rows/sec with indices ([benchmark](https://www.getgalaxy.io/learn/glossary/duckdb-vs-sqlite-benchmarks)). FileID writes ~150 files/sec. We use 0.4% of insert capacity.
- Concurrent reads: WAL mode allows app reads while engine writes, no blocking.
- FTS5 + indexes + JSON1 + transactions — all native. No extension friction.
- Single-file portability and `jq`-able introspection via `sqlite3` CLI.

The 1.84M-pair UI lag was a transaction-per-pair shape problem (now fixed by `mergePersonsBatch`), not an SQLite limit. The face clustering wall time was an embedding + algorithm problem (M2 fixes it). Neither was solved by switching engines.

That said — there are two **narrow** places where a different storage could help. They're cataloged below.

---

## What we evaluated

### Option A — DuckDB

**Strengths**
- Up to 50× faster than SQLite on analytical queries (joins, aggregations) at large scale ([motherduck](https://motherduck.com/learn/duckdb-vs-sqlite-databases/)).
- MVCC + morsel-driven parallelism — true concurrent readers + writers without WAL contention.
- Apache-2.0, single C++ library, embeddable.
- **First-class Swift bindings** — [`duckdb/duckdb-swift`](https://github.com/duckdb/duckdb-swift) on SwiftPM, native Sendable conformance, strongly-typed result sets. Better Swift ergonomics than GRDB in some ways.

**Weaknesses for FileID**
- **4k naive inserts/sec** vs. SQLite's 30-40k ([benchmark](https://www.getgalaxy.io/learn/glossary/duckdb-vs-sqlite-benchmarks)). Our scan loop is write-heavy by definition; we'd take a 7-10× write hit on the hot path.
- OLAP-shaped storage (columnar Parquet-like). Optimized for "scan all rows, aggregate" — pessimal for FileID's "fetch one file's metadata by id" pattern.
- Vector extension (VSS) is officially flagged experimental for persistence; WAL recovery for HNSW indexes is incomplete per their own docs.
- No FTS5-equivalent. We'd lose OCR + filename search or have to build it ourselves.

**Verdict.** Wrong shape for the hot path. **Possibly worth re-evaluating** in 6-12 months IF (a) Cleanup duplicate aggregation queries become measurably slow on 1M+ libraries AND (b) DuckDB-VSS exits experimental status. Today: pass.

### Option B — LMDB

**Strengths**
- 47× SQLite on sequential reads, 9× on random reads ([benchmark](https://github.com/marvelousmlops/database_comparison)).
- Memory-mapped — data accessed directly from disk pages, no userspace copy.
- Multi-process concurrent readers scale linearly by design.
- Swift wrapper exists ([SwiftLMDB](https://github.com/agisboye/SwiftLMDB)).

**Weaknesses for FileID**
- **Pure key-value store.** No SQL, no joins, no FTS5, no aggregations.
- We'd hand-build relational queries on top — every Library page, every Cleanup duplicate group, every Restructure proposal becomes a manual index walk.
- We'd hand-build full-text search (the OCR FTS5 search is a load-bearing UX feature).
- Schema migrations become bespoke.

**Verdict.** Strict downgrade for FileID's workload. The "47× faster reads" only matters when reads are slow; ours aren't. Pass.

### Option C — LanceDB

**Strengths**
- Built specifically for vectors. Apache Arrow columnar + SIMD + Apple Silicon GPU (MPS) support for index training.
- Embedded mode (no separate server).
- Versioned snapshots — interesting for "show me the library state from before a re-cluster."

**Weaknesses for FileID**
- Swift bindings are community / custom FFI. No first-party Apple support. Real maintenance liability for a one-person codebase.
- Designed for >10M vector libraries. Below that, the Arrow tax outweighs the gains.
- We'd run TWO databases (LanceDB for vectors, SQLite for metadata). Cross-store consistency becomes a thing.

**Verdict.** Right for >10M vectors with a team to maintain bindings. Not for FileID today.

### Option D — sqlite-vec / vectorlite (SQLite extensions)

These keep SQLite as the storage engine but add proper vector indexes.

| Extension | Index type | Inserts | Queries | When to use |
|-----------|------------|---------|---------|-------------|
| **sqlite-vec** | Brute-force only | Fast | O(N) per query | < 100k vectors |
| **vectorlite** | HNSW (ANN) | 6-16× slower than sqlite-vec | **3-100× faster** at large N | > 100k vectors with query latency demands |

([source](https://github.com/1yefuwang1/vectorlite))

**For FileID:**
- ArcFace embeddings: 50K faces × 2 KB = 100 MB. Brute-force cosine over 50k vectors in pure Swift is ~30 ms. We don't need an index.
- MobileCLIP image embeddings: 60k images × 2 KB = 120 MB. Same story.
- We hit "need ANN index" territory at ~500k+ embeddings or sub-50ms query latency demands. Not there yet.

**Verdict.** **Worth adding when the user crosses ~500k images.** Not urgent. Vectorlite drops in alongside SQLite via `sqlite3_load_extension` — no migration risk, just a `.dylib` to vendor.

### Option E — keep SQLite, optimize what we have

The honest path. Most "DB feels slow" symptoms in this codebase have been algorithm or transaction-shape problems:

| Past symptom | Was it the DB? | Actual cause | Fix |
|--------------|----------------|--------------|-----|
| 1.84M Suggested Merges sheet sluggish | No | Per-pair `DatabaseQueue` + transaction | `mergePersonsBatch` — single transaction with union-find |
| Face clustering wall time | No | Vision feature print embeddings + greedy HNSW | M2: ArcFace + Chinese Whispers |
| Library grid slow on big libraries | No | App rendered all rows at once | LazyVGrid + paged `files()` query |
| Cleanup duplicate query slow | Marginal | `GROUP BY phash` over 60k rows runs in ~50ms | Indexed phash column already exists |

When SQLite has been close to a real bottleneck:
- WAL file growth degrading reads — handled by `wal_autocheckpoint = 10000`.
- Cross-process contention (engine writer + app brief writer for Cleanup deletes) — measured at <5ms wait, fine.

---

## Recommendations

**Immediate (zero migration risk):**

1. **Keep SQLite + GRDB.** Continue treating "this query is slow" as "investigate the query / index / transaction shape" before reaching for a different engine.
2. **Add an `EXPLAIN QUERY PLAN` lint** to the read paths — catch missing indexes before users hit them.
3. **Periodically `PRAGMA optimize`** at engine startup. GRDB doesn't do this automatically; it materially improves query planning on tables that have grown a lot since the last `ANALYZE`.

**Conditional (only if perf actually demands it):**

4. **Vectorlite** when the library crosses ~500k face or image embeddings AND the query latency exceeds 50ms. Drop-in extension; no schema migration; revert is one line.
5. **DuckDB-VSS** to revisit in 6-12 months. Only attractive if (a) it exits experimental for persistence and (b) we have an analytical workload that's actually slow.

**Pass:**

6. **LMDB** — wrong abstraction.
7. **LanceDB** — too soon, bindings risk too high.
8. **Postgres / MySQL / any client-server DB** — kills the "single-file local data" property; out of scope.

---

## Where the actual perf headroom is

If "I want FileID to feel faster" is the goal, the highest-impact levers in priority order:

1. **Finish M3 migration** — running ArcFace + Chinese Whispers on the user's library. Their current pain is here.
2. **Pre-compute the Cleanup duplicate groups on a `BatchSummary` ticker** instead of recomputing per-tab-open. ~50ms → ~5ms perceived.
3. **Pin the engine to performance cores** via `qos` hints. Modest ANE saturation gain.
4. **Bump `mmap_size`** from 256 MB to 1 GB on Macs with 32+ GB RAM. ~10-20% read speedup on warm queries.
5. **Vectorlite** — at the right scale, see "Conditional" above.

None of these requires a different database.

---

## Sources

- [DuckDB vs SQLite benchmarks](https://www.getgalaxy.io/learn/glossary/duckdb-vs-sqlite-benchmarks)
- [DuckDB vs SQLite (motherduck)](https://motherduck.com/learn/duckdb-vs-sqlite-databases/)
- [DuckDB Swift bindings](https://github.com/duckdb/duckdb-swift)
- [LMDB vs SQLite benchmark](https://github.com/marvelousmlops/database_comparison)
- [vectorlite vs sqlite-vec performance](https://github.com/1yefuwang1/vectorlite)
- [SQLite WAL concurrency limits](https://oldmoe.blog/2024/07/08/the-write-stuff-concurrent-write-transactions-in-sqlite/)
- [SQLite WAL official docs](https://sqlite.org/wal.html)
- [LanceDB embedded mode](https://www.lancedb.com/)
