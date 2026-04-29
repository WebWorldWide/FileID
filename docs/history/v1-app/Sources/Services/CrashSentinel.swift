import Foundation

// MARK: - CrashSentinel
//
// An always-on "what is the app doing right now" marker, written to disk
// atomically. The file is created at app launch and cleared on graceful
// exit (AppDelegate.applicationWillTerminate). If the app hard-crashes
// (SIGABRT, SIGKILL, runtime metadata abort) the file survives, and the
// NEXT launch reads it as an "orphan" marker — giving us a forensic trail
// that tells us exactly which phase / fileID the previous run was on
// when it died.
//
// The file is NOT the scan log. scan.log is an append-only transcript;
// this is a single-line "last known state" snapshot. They complement
// each other: scan.log tells you what happened up to the crash, the
// sentinel tells you which operation was in flight at the moment of death.
//
// Design notes:
//   - Codable JSON at ~/Library/Application Support/FileID/app_running.json
//   - PID stored so readOrphan() can distinguish "the current process
//     already wrote this marker" from "a different process died and left it"
//   - Writes are atomic (Data.write(options: .atomic)) so a SIGABRT during
//     a write cannot produce a half-finished JSON blob for the next launch
//   - No actor / queue — callers are mostly already on a serialized executor
//     (MediaProcessor actor, FaceClusteringService @ModelActor). A stray
//     concurrent write loses the earlier snapshot, but we only care about
//     the latest anyway.

enum CrashSentinel {

    struct Marker: Codable {
        var startedAt: Date
        var phase:     String         // "launch", "discovery", "vision",
                                      // "naming", "clustering", "rebuildIndex",
                                      // "deep-analyze", "idle"
        var subject:   String?        // human-readable: fileID, URL, batch#
        var lastBatch: Int?
        var pid:       Int32
    }

    private static let path: URL = {
        let base = FileManager.default.urls(for: .applicationSupportDirectory,
                                            in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        let dir = base.appendingPathComponent("FileID", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("app_running.json")
    }()

    // MARK: - Public API

    /// Set the current phase / subject. Safe to call from any isolation
    /// context; the underlying write is atomic.
    static func set(phase: String, subject: String? = nil, batch: Int? = nil) {
        let marker = Marker(
            startedAt: Date(),
            phase:     phase,
            subject:   subject,
            lastBatch: batch,
            pid:       getpid()
        )
        guard let data = try? JSONEncoder().encode(marker) else { return }
        try? data.write(to: path, options: .atomic)
    }

    /// Remove the marker. Called from `applicationWillTerminate` so a clean
    /// quit doesn't leave an orphan that confuses next launch's crash check.
    static func clear() {
        try? FileManager.default.removeItem(at: path)
    }

    /// Returns the marker IFF it was left by a previous process (pid != ours).
    /// At launch this indicates the previous run died before calling `clear`.
    static func readOrphan() -> Marker? {
        guard let data = try? Data(contentsOf: path),
              let marker = try? JSONDecoder().decode(Marker.self, from: data)
        else { return nil }
        if marker.pid == getpid() { return nil }  // we wrote this ourselves
        return marker
    }
}
