# Scheduling and model residency: implementation status

The native major-job queue still executes one heavy Deep Analyze job at a time. Queued interactive chat summaries precede pending background work, but do not preempt an active job. Timeline work persists checkpoints and restarts paused; general jobs do not yet use a durable scheduler. The process-wide ResourceScheduler coordinates modeled memory, CPU, and I/O claims on macOS; it is not a replacement for the job queue.

## Initial model admission

Both engines reserve 25% of physical RAM, bounded to 2–8 GiB, when checking whether a model can fit. They also require 0.5–2 GiB of available headroom after the requested allocation; failed probes, zero estimates, oversized models, and current pressure reject loading with an actionable message. Shared fixtures cover 4/8/16/64 GiB systems, missing probes, and integer extremes. These are conservative initial budgets, not measured quality/speed guarantees.

macOS uses the existing model registry's RAM estimates and checks current available memory after releasing a previous model when a switch fits physical capacity. Requests that cannot fit total RAM leave the previous container intact. The memory probe counts free plus inactive pages once: Apple's XNU vm_statistics64 header explicitly includes speculative pages in free_count.

A cancellable exclusive gate owns macOS loading, unloading, catalog summaries, face comparisons, and image/document analysis. A model swap waits for active inference to finish or unwind; queued cancellation removes its waiter without releasing another task's active lease. Cancellation during handoff is regression-tested. Existing single-flight loading and last-waiter download cancellation remain in place.

Rust checks GGUF plus projection file size with 25% overhead and a 1 GiB workspace allowance before both persistent-server and per-file subprocess startup. This is a conservative host-memory estimate. It does not measure free dedicated GPU memory, enforce shared reservations across arbitrary simultaneous subprocesses, or establish memory requirements for every quantization/context. Dedicated-GPU and context-specific calibration remain required.

Decoder watchdogs use a dispatch timer rather than a cooperative Task sleep. Full native testing exposed a deadline delay under executor load; the original five-second assertion for a one-second watchdog remains in place. Cancellation still kills the owned worker and awaits termination. This does not establish that every native inference call is cancellable; isolated inference workers remain pending.

## Continue implementing

Continue the scheduler by covering every heavy file pipeline, adding bounded checkpoints/yields and restart-safe admission across job kinds, and measuring CPU pressure, storage throughput, and interactive latency. The current CPU/I/O units are concurrency permits, not load or bandwidth measurements. Apple unified GPU/ANE memory is charged against the same process-memory budget; no separate accelerator budget is observable here. Add model/task routing, context-prefix reuse, independent model residency, capability reporting, and validated CPU/GPU/NPU fallbacks only after measurements. The exclusive MLX gate and one resident VLM remain in place.


## Shared macOS resource admission — October 2026

`ResourceScheduler` is one process-wide actor shared by model work and the native catalog index. It atomically reserves estimated process memory, CPU concurrency units, and I/O concurrency units. Memory uses the existing 25% physical-RAM reserve (bounded 2–8 GiB) and 0.5–2 GiB free floor. CPU capacity is `max(1, activeProcessorCount - 2)`; the engine allows two I/O units. These are conservative admission limits, not measurements of actual CPU utilization, I/O bandwidth, or thermal pressure.

Index rebuilds request one memory/CPU/I/O unit at background priority. VLM loading reserves its estimated resident memory and one CPU/I/O unit, then releases CPU/I/O while retaining its memory lease. Catalog chat, VLM analysis, and face comparison request one interactive CPU unit. Interactive waiters precede queued background work; an admitted worker is not preempted. Background indexing that cannot fit beside interactive resident memory is paused as a durable, resumable catalog job. User cancellation removes its waiter without releasing another job’s lease.

On Apple Silicon, GPU and Neural Engine allocations use unified memory and count toward the same process-memory budget; the scheduler has no separate accelerator-memory telemetry. The actor itself is process-local; the SQLite catalogIndex record owns restart recovery. Only the index worker and DeepAnalyze loading/chat/analysis/face-comparison paths currently use these permits. File scanning, ONNX face/tag workers, every conversion path, and other job kinds still need scheduler integration. The current DeepAnalyze actor still holds one model container and serializes model operations.

## Native catalog index jobs — October 2026

Native CLIP preparation now records a `catalogIndex` job in the existing v21 catalog_jobs table. One actor coalesces work, transfers graph ownership to a utility worker and persists bounded progress checkpoints. Pause/cancel stop the worker and await its termination; explicit resume also retries failed/cancelled index jobs without a visual model. Startup converts interrupted queued/running index jobs to paused. Tools polls job snapshots while open. Keyword search remains available.

Restart/retry uses the last verified graph/manifest and SQLite generation/nonce history, never a partially populated graph or cursor from checkpoint_json. Checkpoint counts are progress evidence. Cache reads, compaction, and snapshot loops check cancellation; graph/manifest publication follows the existing atomic-file sequence after a final cancellation check. Index rebuilding obtains a background memory/CPU/I/O lease from the shared process scheduler and holds it through graph publication. If the request cannot fit beside resident interactive work, the durable catalogIndex job pauses and can resume after resources are freed. Scheduler leases are process-local and are not restored after restart.
