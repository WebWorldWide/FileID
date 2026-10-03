# Native vector-index benchmark

Run from the repository root on macOS. This compiles the actual HNSW source in release mode and the actual read-only path guard as a minimal shared module. It does not run the catalog, model inference, IPC, UI or corpus analysis. No model download is needed. Outputs stay in the internal `/tmp` directory; never substitute an Adlon path.

```sh
work_dir="$(mktemp -d /tmp/fileid-vector-bench.XXXXXX)"
xcrun swiftc -O -emit-library -emit-module -module-name FileIDShared \
  platforms/apple/shared/Sources/FileIDShared/ReadOnlyLocations.swift \
  -o "$work_dir/libFileIDShared.dylib" \
  -emit-module-path "$work_dir/FileIDShared.swiftmodule"
xcrun swiftc -O -parse-as-library -I "$work_dir" -L "$work_dir" -lFileIDShared \
  -Xlinker -rpath -Xlinker "$work_dir" \
  platforms/apple/engine/Sources/FileIDEngine/Models/HNSWIndex.swift \
  platforms/apple/benchmarks/VectorIndexBenchmark.swift -o "$work_dir/benchmark"
"$work_dir/benchmark" 100000 512 256 > "$work_dir/results.json"
cat "$work_dir/results.json"
```

Arguments are vector count (100–100,000), dimension (16–512) and search beam (16–1,024). The deterministic dataset contains 64 noisy normalized clusters; 20 unseen queries are compared with exact top-ten neighbours. Results include mean/minimum recall, warm index-only p95 latency, build/save/load times, snapshot bytes and equality after restoration. Keep raw JSON, hardware, compiler, source revision and parameters together. This synthetic result does not establish real semantic or face accuracy, end-to-end retrieval latency, or release acceptance.

The smaller regression fixture uses 16,000 × 128 vectors and beam 64. Closest-only pruning scored 57% mean recall with zero-recall queries; diversified insertion and pruning scored 100% on the same fixture. The 100,000 × 512 experiment improved from 62.5% to 100% with beam 256. These are twenty-query observations on one deterministic fixture, not universal accuracy guarantees. See `shared/docs/STATE.md` for the run and remaining integration work.
