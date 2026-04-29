import Foundation

// MARK: - ClusterCircuitBreaker

// Breaks the stale-face-print feedback loop that crashed the app three times
// in a row on the 58 K TrueNAS library.
//
// The loop: face clustering crashes mid-batch (cause: ImageIO NSException /
// jetsam / vDSP OOB — we've seen all three). MediaProcessor.runFaceClusteringPass
// only calls FacePrintCache.remove(id) AFTER clusterBatch returns successfully,
// so the crashing print stays on disk. On relaunch, the same hasFaces==true
// query reloads the same prints, clusterSync hits the same crasher, and the
// app dies again. Forever.
//
// The breaker persists two pieces of state:
//   1. An in-flight marker — the fileID being clustered right now. Written
//      atomically before each clusterSync call, deleted on success. If the
//      app crashes mid-call, the marker survives the next launch.
//   2. A permanent skip-list — fileIDs whose prints crashed 3+ times and will
//      never be clustered again (FacePrintCache entries also deleted). The
//      user can reset the list from Settings once a code fix has shipped.
//
// At app launch, `recoverFromCrash` inspects the in-flight marker. If it
// exists, that fileID crashed on its last attempt — increment the attempt
// count, and if it's now >= threshold, move it to the permanent skip-list
// AND nuke its FacePrintCache entries (that's what actually breaks the loop).
//
// This is an actor so `beginAttempt` / `markSuccess` are safely callable
// from the FaceClusteringService's @ModelActor executor, and so concurrent
// calls to `recoverFromCrash` at launch can't race.

actor ClusterCircuitBreaker {

    // Shared instance. `recoverFromCrash` must be awaited once at app launch
    // (wired from FileIDApp.swift) before any clustering runs, otherwise an
    // in-flight marker from a prior crash won't be escalated.
    static let shared = ClusterCircuitBreaker()

    private static let maxAttempts: Int = 3

    private init() {}

    // MARK: - Storage paths

    private static var stateDir: URL {
        let base = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        let dir = base.appendingPathComponent("FileID", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private static var inflightURL: URL { stateDir.appendingPathComponent("cluster_inflight.json") }
    private static var skipListURL: URL { stateDir.appendingPathComponent("bad_face_prints.json") }

    // MARK: - Codable state

    private struct InFlight: Codable {
        var fileID: UUID
        var attempts: Int
    }

    private struct SkipList: Codable {
        var ids: [UUID]
    }

    // MARK: - In-memory mirror of the persisted skip-list.

    // Loaded lazily on first access, updated on every add. Reads are O(1)
    // because callers check membership via contains() in a tight loop.
    private var cachedSkipSet: Set<UUID>?

    private func loadSkipSet() -> Set<UUID> {
        if let cached = cachedSkipSet { return cached }
        guard let data = try? Data(contentsOf: Self.skipListURL),
              let list = try? JSONDecoder().decode(SkipList.self, from: data) else {
            let empty: Set<UUID> = []
            cachedSkipSet = empty
            return empty
        }
        let s = Set(list.ids)
        cachedSkipSet = s
        return s
    }

    private func writeSkipSet(_ set: Set<UUID>) {
        cachedSkipSet = set
        let list = SkipList(ids: Array(set))
        guard let data = try? JSONEncoder().encode(list) else { return }
        try? data.write(to: Self.skipListURL, options: .atomic)
    }

    // MARK: - Public API

    /// Snapshot of the current skip-list. MediaProcessor uses this to filter
    /// the `Pending` chunk before loading prints.
    func skipList() -> Set<UUID> {
        loadSkipSet()
    }

    /// Return true if this file's prints should be skipped outright.
    func shouldSkip(_ fileID: UUID) -> Bool {
        loadSkipSet().contains(fileID)
    }

    /// Record that we're about to call clusterSync for this file. Written
    /// atomically so a crash between write + clusterSync leaves a recoverable
    /// marker on disk. Returns the attempt count (1 for a first attempt).
    @discardableResult
    func beginAttempt(fileID: UUID) -> Int {
        // Preserve prior attempt count if the marker survived a crash AND
        // hasn't been escalated yet (recoverFromCrash would have moved it).
        let prior: Int = {
            guard let data = try? Data(contentsOf: Self.inflightURL),
                  let inflight = try? JSONDecoder().decode(InFlight.self, from: data),
                  inflight.fileID == fileID else { return 0 }
            return inflight.attempts
        }()
        let next = prior + 1
        let marker = InFlight(fileID: fileID, attempts: next)
        if let data = try? JSONEncoder().encode(marker) {
            try? data.write(to: Self.inflightURL, options: .atomic)
        }
        return next
    }

    /// Mark the in-flight file as successfully clustered. Clears the marker
    /// so the next crash won't wrongly blame it.
    func markSuccess(fileID: UUID) {
        guard let data = try? Data(contentsOf: Self.inflightURL),
              let inflight = try? JSONDecoder().decode(InFlight.self, from: data),
              inflight.fileID == fileID else {
            // Marker either missing or belongs to a different file (another
            // worker updated it). Don't clobber either way.
            return
        }
        try? FileManager.default.removeItem(at: Self.inflightURL)
    }

    /// Called at app launch. If an in-flight marker exists it means the last
    /// clusterSync attempt never completed (the app was killed mid-call).
    /// If the attempt count has reached the threshold, escalate this fileID
    /// to the permanent skip-list AND remove its FacePrintCache entries so
    /// the next scan can't resurrect the same crashing print.
    ///
    /// Returns the fileID that was escalated (for logging), or nil.
    @discardableResult
    func recoverFromCrash() -> UUID? {
        guard let data = try? Data(contentsOf: Self.inflightURL),
              let inflight = try? JSONDecoder().decode(InFlight.self, from: data) else {
            return nil
        }

        // The marker's attempts value was written BEFORE the crash, so it
        // reflects the number of times we've tried WITHOUT yet succeeding.
        // If that's already at or past the threshold, escalate.
        if inflight.attempts >= Self.maxAttempts {
            var set = loadSkipSet()
            set.insert(inflight.fileID)
            writeSkipSet(set)
            // Nuke the on-disk prints so a future scan can't re-feed them
            // into clusterBatch. This is what actually breaks the loop.
            FacePrintCache.remove(inflight.fileID)
            try? FileManager.default.removeItem(at: Self.inflightURL)
            let msg = "ClusterCircuitBreaker: permanently skipping fileID=\(inflight.fileID.uuidString) after \(inflight.attempts) crashed attempts"
            NSLog("FileID %@", msg)
            // Mirror to scan.log so the cumulative quarantine trail is
            // visible in the same log the user already inspects.
            MediaProcessor.appendScanLogExternal(msg)
            return inflight.fileID
        }

        // Below threshold — leave the marker in place so beginAttempt sees
        // it and increments. Next clusterSync call will count as attempt N+1.
        NSLog("FileID ClusterCircuitBreaker: in-flight marker survived from prior crash for fileID \(inflight.fileID.uuidString) (attempt \(inflight.attempts) of \(Self.maxAttempts))")
        return nil
    }

    /// User-facing reset. Called from Settings → "Reset face-clustering skip-list".
    func resetSkipList() {
        cachedSkipSet = []
        try? FileManager.default.removeItem(at: Self.skipListURL)
        try? FileManager.default.removeItem(at: Self.inflightURL)
    }

    /// For UI display — count shown next to the reset button.
    func skippedCount() -> Int {
        loadSkipSet().count
    }
}
