// swift-tools-version: 6.0
import PackageDescription

// FileID — workspace package.
//
// One app + the shared modules the Rust engine's adapters use:
// - FileIDShared: Codable DTOs, the IPC protocol and the few read-only
//                 catalog/model routines the app runs.
// - FileIDImaging: decode, Vision and MLX image code FileIDAppleKit (the
//                 Rust engine's adapters) runs.
// - FileID:       SwiftUI app — spawns `fileidd`, the shared Rust engine
//                 (platforms/windows/src/engine, --features apple-adapters),
//                 as a child process via the Foundation `Process` API. The
//                 app's lifetime owns the engine's lifetime.
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
        // Swift adapters the shared Rust engine (`Contents/MacOS/fileidd`)
        // links through the C interface in adapters/include/fileid_apple.h.
        // Shipped in Contents/Frameworks. Links MLX for the VLM sessions, CLIP and RAM++.
        .library(name: "FileIDAppleKit", type: .dynamic, targets: ["FileIDAppleKit"])
    ],
    dependencies: [
        // VLM inference for Deep Analyze + AI face clustering.
        .package(url: "https://github.com/ml-explore/mlx-swift-lm", exact: "3.32.3"),
        .package(url: "https://github.com/huggingface/swift-transformers", from: "1.3.0"),
        // SQLite (WAL-mode), read-only in the app (ReadStore).
        .package(url: "https://github.com/groue/GRDB.swift", from: "7.0.0")
    ],
    targets: [
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

        // Decode, Vision, text-read and MLX image code FileIDAppleKit exposes
        // to the Rust engine. No package dependency beyond MLX.
        .target(
            name: "FileIDImaging",
            dependencies: [
                .product(name: "MLXLMCommon", package: "mlx-swift-lm")
            ],
            path: "shared/Sources/FileIDImaging",
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),

        // MLX vision-language code (Deep Analyze, chat, selection checks,
        // benchmarks, timeline captions), run by FileIDAppleVLM. Host services
        // come in through VLMCoreHost hooks; no FileIDShared, so no download
        // code. Production targets compile in Swift 6 mode; concurrency
        // diagnostics are release-blocking rather than deferred to code review.
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

        // SwiftUI app. Spawns fileidd via the Process API.
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
        // functions only; see adapters/include. Scan work runs FileIDImaging
        // (decode, Vision and MLX code); VLM sessions run FileIDAppleVLM.
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
