// swift-tools-version: 6.0
import PackageDescription

// FileID — workspace package.
//
// Two products + shared modules:
// - FileIDShared: Codable DTOs, the IPC protocol and the few read-only
//                 catalog/model routines both the engine and the app run.
// - FileIDImaging: decode, Vision and MLX image code the Swift engine and
//                 FileIDAppleKit (the Rust engine's adapters) both run.
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
    products: [
        .executable(name: "FileID", targets: ["FileID"]),
        .executable(name: "FileIDEngine", targets: ["FileIDEngine"]),
        // Swift adapters the shared Rust engine (`Contents/MacOS/fileidd`)
        // links through the C interface in adapters/include/fileid_apple.h.
        // Shipped in Contents/Frameworks. Links MLX for the VLM sessions, CLIP and RAM++.
        .library(name: "FileIDAppleKit", type: .dynamic, targets: ["FileIDAppleKit"])
    ],
    dependencies: [
        // VLM inference for Deep Analyze + AI face clustering.
        .package(url: "https://github.com/ml-explore/mlx-swift-lm", exact: "3.32.3"),
        .package(url: "https://github.com/huggingface/swift-transformers", from: "1.3.0"),
        // Bounded AsyncChannel for the backpressured streaming pipeline.
        .package(url: "https://github.com/apple/swift-async-algorithms", from: "1.0.0"),
        // SQLite (WAL-mode) — the engine's primary store. Explicit
        // transaction control; FTS5 + vectorlite extension support.
        .package(url: "https://github.com/groue/GRDB.swift", from: "7.0.0"),
        // ONNX Runtime for the retired Swift engine (conformance peer only).
        // Only FileIDORTSupport and FileIDEngine link it; the shipped app and
        // the shared Rust engine's adapters do not.
        .package(path: "vendor/onnxruntime-swift-package-manager")
    ],
    targets: [
        // ONNX Runtime C API calls the Objective-C bindings don't expose.
        // Linked only by FileIDORTSupport, so only the Swift engine.
        .target(
            name: "FileIDORTC",
            dependencies: [
                .product(name: "onnxruntime", package: "onnxruntime-swift-package-manager")
            ],
            path: "shared/Sources/FileIDORTC"
        ),
        // Model kinds, the MLX model catalog, model paths and file kinds:
        // Foundation only (no download, ONNX Runtime or MLX code), so the MLX
        // code in FileIDVLMCore and FileIDAppleKit can use them.
        .target(
            name: "FileIDModelInfo",
            path: "shared/Sources/FileIDModelInfo",
            exclude: ["Resources"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .target(
            name: "FileIDShared",
            dependencies: [
                "FileIDModelInfo",
                .product(name: "GRDB", package: "GRDB.swift")
            ],
            path: "shared/Sources/FileIDShared",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        // The ONNX Runtime code only the Swift engine runs: the telemetry-off
        // environment and the CLIP text encoder. The shipped app and
        // FileIDShared link none of it; text embeddings for the app come from
        // the shared Rust engine over IPC.
        .target(
            name: "FileIDORTSupport",
            dependencies: [
                "FileIDShared",
                "FileIDORTC",
                .product(name: "onnxruntime", package: "onnxruntime-swift-package-manager")
            ],
            path: "shared/Sources/FileIDORTSupport",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        // Decode, Vision, text-read and MLX image code both engines run: the
        // Swift engine links it directly, FileIDAppleKit exposes it to the
        // Rust engine. No package dependency beyond the MLX the two already link.
        .target(
            name: "FileIDImaging",
            dependencies: [
                .product(name: "MLXLMCommon", package: "mlx-swift-lm")
            ],
            path: "shared/Sources/FileIDImaging",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        // Engine CLI. Single binary, owns the scan pipeline + Deep Analyze
        // (VLM inference via MLX). MLXVLM brings the Qwen / Gemma /
        // PaliGemma model factory; downloaded weights are cached in
        // Developer builds use `~/Documents/huggingface/models/<repo>/`; the App Store build uses the app container.
        //
        // Production targets compile in Swift 6 mode; concurrency diagnostics
        // are release-blocking rather than deferred to code review.
        // The Swift engine's MLX vision-language code (Deep Analyze, chat,
        // selection checks, benchmarks, timeline captions), shared with
        // FileIDAppleKit. Host services come in through VLMCoreHost hooks; no
        // FileIDShared, so no download or ONNX Runtime code.
        .target(
            name: "FileIDVLMCore",
            dependencies: [
                "FileIDModelInfo",
                "FileIDImaging",
                .product(name: "MLXLMCommon",          package: "mlx-swift-lm"),
                .product(name: "MLXVLM",               package: "mlx-swift-lm"),
                .product(name: "MLXLLM",               package: "mlx-swift-lm"),
                .product(name: "MLXHuggingFace",       package: "mlx-swift-lm"),
                .product(name: "Tokenizers",           package: "swift-transformers")
            ],
            path: "shared/Sources/FileIDVLMCore",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        .executableTarget(
            name: "FileIDEngine",
            dependencies: [
                "FileIDShared",
                "FileIDORTSupport",
                "FileIDImaging",
                "FileIDVLMCore",
                .product(name: "AsyncAlgorithms",      package: "swift-async-algorithms"),
                .product(name: "GRDB",                 package: "GRDB.swift"),
                .product(name: "MLXLMCommon",          package: "mlx-swift-lm"),
                .product(name: "MLXVLM",               package: "mlx-swift-lm"),
                .product(name: "MLXLLM",               package: "mlx-swift-lm"),
                .product(name: "MLXHuggingFace",       package: "mlx-swift-lm"),
                .product(name: "Tokenizers",           package: "swift-transformers"),
                .product(name: "onnxruntime",          package: "onnxruntime-swift-package-manager")
            ],
            path: "engine/Sources/FileIDEngine",
            swiftSettings: [.swiftLanguageMode(.v6)]
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
            resources: [.copy("Resources/Fonts")],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        // MLX sessions for the Rust engine: FileIDVLMCore behind the request
        // surface the C exports call, with the Rust host's offline model check.
        .target(
            name: "FileIDAppleVLM",
            dependencies: [
                "FileIDModelInfo",
                "FileIDImaging",
                "FileIDVLMCore",
                .product(name: "MLXLMCommon",          package: "mlx-swift-lm")
            ],
            path: "adapters/Sources/FileIDAppleVLM",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        // Apple-framework adapters for the Rust engine. Exports @_cdecl
        // functions only; see adapters/include. Scan work runs FileIDImaging,
        // the same decode, Vision and MLX code the Swift engine runs; VLM
        // sessions run FileIDAppleVLM.
        .target(
            name: "FileIDAppleKit",
            dependencies: [
                "FileIDModelInfo",
                "FileIDImaging",
                "FileIDAppleVLM",
                .product(name: "MLXLMCommon", package: "mlx-swift-lm")
            ],
            path: "adapters/Sources/FileIDAppleKit",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        .testTarget(
            name: "AppleKitTests",
            dependencies: [
                "FileIDAppleKit", "FileIDAppleVLM", "FileIDVLMCore", "FileIDImaging", "FileIDModelInfo", "FileIDShared",
                .product(name: "GRDB", package: "GRDB.swift"),
                .product(name: "MLXVLM", package: "mlx-swift-lm")
            ],
            path: "Tests/AppleKitTests",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "SharedTests",
            dependencies: ["FileIDShared", "FileIDModelInfo"],
            path: "Tests/SharedTests",
            // PEM fixtures are read via #filePath traversal (the SPKI
            // tests), not Bundle resources.
            exclude: ["Fixtures"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "EngineTests",
            dependencies: ["FileIDEngine", "FileIDORTSupport", "FileIDVLMCore"],
            path: "Tests/EngineTests",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "FileIDAppTests",
            dependencies: [
                "FileID",
                .product(name: "GRDB", package: "GRDB.swift")
            ],
            path: "Tests/FileIDAppTests",
            swiftSettings: [.swiftLanguageMode(.v6)]
        )
    ]
)
