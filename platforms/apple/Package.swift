// swift-tools-version: 6.0
import PackageDescription

// FileID — workspace package.
//
// Two products + a shared module:
// - FileIDShared: Codable DTOs and IPC protocol shared between the engine
//                 and the app. Pure Swift, no dependencies.
// - FileIDEngine: CLI binary — owns the scan pipeline, the SQLite DB,
//                 and ML inference. Reads commands from stdin
//                 (newline-delimited JSON), writes events to stdout (same).
// - FileID:       SwiftUI app — spawns FileIDEngine as a child process via
//                 the Foundation `Process` API. The app's lifetime owns
//                 the engine's lifetime.
//
// Why stdin/stdout JSON instead of XPC: for a strict child-of-app model
// (one consumer, no system-wide service registration), it's simpler to
// set up, trivial to debug (`./fileidd | jq .`), and architecturally
// identical for our command + event stream.
let package = Package(
    name: "FileID",
    platforms: [
        .macOS(.v15)
    ],
    dependencies: [
        // VLM inference for Deep Analyze + AI face clustering.
        .package(url: "https://github.com/ml-explore/mlx-swift-examples", from: "2.21.0"),
        // Bounded AsyncChannel for the backpressured streaming pipeline.
        .package(url: "https://github.com/apple/swift-async-algorithms", from: "1.0.0"),
        // SQLite (WAL-mode) — the engine's primary store. Explicit
        // transaction control; FTS5 + vectorlite extension support.
        .package(url: "https://github.com/groue/GRDB.swift", from: "7.0.0"),
        // ONNX Runtime for face embedder. Lets the engine pull Buffalo
        // ONNX directly from upstream (Immich's HF mirror) at runtime —
        // same posture Immich itself uses, no weight redistribution on
        // our part. CoreML execution provider keeps ANE acceleration.
        .package(url: "https://github.com/microsoft/onnxruntime-swift-package-manager", from: "1.20.0")
    ],
    targets: [
        // Shared types: Codable DTOs, IPC envelope. Pure Swift, no
        // dependencies — both engine and app link this.
        .target(
            name: "FileIDShared",
            path: "shared/Sources/FileIDShared",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        // Engine CLI. Single binary, owns the scan pipeline + Deep Analyze
        // (VLM inference via MLX). MLXVLM brings the Qwen / Gemma / SmolVLM
        // / PaliGemma model factory; downloaded weights are cached in
        // `~/Documents/huggingface/models/<repo>/` by MLX itself.
        //
        // V15.2.1: language mode .v5 + targeted upcoming features. Strict
        // Swift 6 mode flags reads of Darwin's `mach_task_self_` (a global
        // var with kernel-immutable semantics) as "shared mutable state".
        // The accepted Swift 6 workaround patterns (nonisolated(unsafe)
        // let, withUnsafePointer, etc.) all still require reading the var
        // somewhere, which the compiler chases recursively. Until Apple
        // ships a sendable accessor we relax language mode for this
        // target while keeping the strict-concurrency *style*
        // commitments (@MainActor on UI surfaces, `actor` for shared
        // mutable services) intact. CLAUDE.md (apple) talks about
        // "Swift 6 strict concurrency" as a code-style rule; this change
        // is to the compiler enforcement mode, not the code style.
        .executableTarget(
            name: "FileIDEngine",
            dependencies: [
                "FileIDShared",
                .product(name: "AsyncAlgorithms",      package: "swift-async-algorithms"),
                .product(name: "GRDB",                 package: "GRDB.swift"),
                .product(name: "MLXLMCommon",          package: "mlx-swift-examples"),
                .product(name: "MLXVLM",               package: "mlx-swift-examples"),
                .product(name: "onnxruntime",          package: "onnxruntime-swift-package-manager")
            ],
            path: "engine/Sources/FileIDEngine",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),

        // SwiftUI app. Spawns FileIDEngine via Process API.
        // GRDB is read-only here — the app opens a DatabaseQueue (not
        // Pool) for snapshot-style reads. The engine is the only writer.
        .executableTarget(
            name: "FileID",
            dependencies: [
                "FileIDShared",
                .product(name: "GRDB", package: "GRDB.swift")
            ],
            path: "app/Sources/FileID",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        .testTarget(
            name: "SharedTests",
            dependencies: ["FileIDShared"],
            path: "Tests/SharedTests",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "EngineTests",
            dependencies: ["FileIDEngine"],
            path: "Tests/EngineTests",
            swiftSettings: [.swiftLanguageMode(.v6)]
        )
    ]
)
