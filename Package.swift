// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "FileID",
    platforms: [
        .macOS(.v14)
    ],
    targets: [
        .executableTarget(
            name: "FileID",
            path: "Sources"
        )
    ]
)
