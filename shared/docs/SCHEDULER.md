# Scheduling and model residency: implementation status

The native major-job queue still executes one heavy job at a time. Queued interactive chat summaries precede pending background work; they do not preempt a running job. Timeline jobs persist checkpoints and restart in a paused state, while general jobs and model residency are not yet a durable resource-budgeted scheduler.

## Initial model admission

Both engines reserve 25% of physical RAM, bounded to 2–8 GiB, when checking whether a model can fit. They also require 0.5–2 GiB of available headroom after the requested allocation; failed probes, zero estimates, oversized models, and current pressure reject loading with an actionable message. Shared fixtures cover 4/8/16/64 GiB systems, missing probes, and integer extremes. These are conservative initial budgets, not measured quality/speed guarantees.

macOS uses the existing model registry's RAM estimates and checks current available memory after releasing a previous model when a switch fits physical capacity. Requests that cannot fit total RAM leave the previous container intact. The memory probe counts free plus inactive pages once: Apple's XNU vm_statistics64 header explicitly includes speculative pages in free_count.

A cancellable exclusive gate owns macOS loading, unloading, catalog summaries, face comparisons, and image/document analysis. A model swap waits for active inference to finish or unwind; queued cancellation removes its waiter without releasing another task's active lease. Cancellation during handoff is regression-tested. Existing single-flight loading and last-waiter download cancellation remain in place.

Rust checks GGUF plus projection file size with 25% overhead and a 1 GiB workspace allowance before both persistent-server and per-file subprocess startup. This is a conservative host-memory estimate. It does not measure free dedicated GPU memory, enforce shared reservations across arbitrary simultaneous subprocesses, or establish memory requirements for every quantization/context. Dedicated-GPU and context-specific calibration remain required.

Decoder watchdogs use a dispatch timer rather than a cooperative Task sleep. Full native testing exposed a deadline delay under executor load; the original five-second assertion for a one-second watchdog remains in place. Cancellation still kills the owned worker and awaits termination. This does not establish that every native inference call is cancellable; isolated inference workers remain pending.

## Continue implementing

Replace the serial lane with durable CPU/storage-I/O/GPU/NPU-memory reservations and model leases, with restart recovery, fair interactive priority, bounded batches, cache limits, and verified capability reporting. Add model/task routing, compatible prompt-prefix/context caching, separate request/model-loading/concurrent-model controls, and CPU fallback for validated combinations. Larger machines may admit independent resident chat/analysis models only after allocation and pressure measurements. Persistent hybrid indexes, portable generation, model-specific optimization/grounding tests, and interactive latency under background contention remain open. Never call the current exclusive gate a parallel multi-model scheduler.


## Native catalog index jobs — October 2026

Native CLIP preparation now records a `catalogIndex` job in the existing v21 catalog_jobs table. One actor coalesces work, transfers graph ownership to a utility worker and persists bounded progress checkpoints. Pause/cancel stop the worker and await its termination; explicit resume also retries failed/cancelled index jobs without a visual model. Startup converts interrupted queued/running index jobs to paused. Tools polls job snapshots while open. Keyword search remains available.

Restart/retry uses the last verified graph/manifest and SQLite generation/nonce history, never a partially populated graph or a cursor from checkpoint_json. Checkpoint counts are progress evidence. Cache reads, compaction and snapshot loops check cancellation; graph/manifest publication follows the existing atomic-file sequence after the final cancellation check. Memory admission estimates live/raw vectors plus cached graph/manifest decoding and snapshot buffers, reserving OS/app headroom. This is conservative admission for one index worker, not a shared atomic CPU/I/O/GPU reservation system or a complete parallel model scheduler. Measure allocations, pressure, fairness and interactive latency before claiming those gates.
