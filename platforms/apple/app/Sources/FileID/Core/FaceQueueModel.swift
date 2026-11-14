import Foundation
import Observation
import FileIDShared

enum FaceQueueText {
    static let mergesEmpty = "No suggested merges right now."
    static let splitsEmpty = "No one looks like two different people right now."
    static func name(_ value: String?) -> String {
        let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed.isEmpty ? "Unnamed" : trimmed
    }
    static func faces(_ count: Int) -> String { "\(count) face\(count == 1 ? "" : "s")" }
    static func mergeReason(_ item: FaceMergeSuggestion) -> String {
        switch FaceReviewText.likelihood(item.score) {
        case "very likely": return "These two are very likely the same person and are never in the same photo."
        case "probably": return "These two are probably the same person and are never in the same photo."
        default: return "These two might be the same person and are never in the same photo."
        }
    }
    static func mergeLabel(_ item: FaceMergeSuggestion) -> String { "Merge \(name(item.otherName)) into \(name(item.personName))" }
    static func notSameLabel(_ item: FaceMergeSuggestion) -> String {
        "\(name(item.otherName)) and \(name(item.personName)) are not the same person"
    }
    static func dismissMergeLabel(_ item: FaceMergeSuggestion) -> String {
        "Hide the suggestion to merge \(name(item.otherName)) and \(name(item.personName))"
    }
    static func mergeRow(_ item: FaceMergeSuggestion) -> String {
        "\(name(item.otherName)), \(faces(item.otherFaceCount)), and \(name(item.personName)), \(faces(item.faceCount)). \(mergeReason(item))"
    }
    static func splitReason(_ item: FaceSplitSuggestion) -> String {
        "These faces form two groups that look like different people."
    }
    static func stays(_ item: FaceSplitSuggestion) -> String { "Stays (\(item.keepCount))" }
    static func moves(_ item: FaceSplitSuggestion) -> String { "Moves to a new person (\(item.splitCount))" }
    static func splitLabel(_ item: FaceSplitSuggestion) -> String { "Split \(name(item.personName)) into two people" }
    static func dismissSplitLabel(_ item: FaceSplitSuggestion) -> String { "Hide the suggestion to split \(name(item.personName))" }
    static func splitRow(_ item: FaceSplitSuggestion) -> String {
        "\(name(item.personName)): \(item.keepCount) faces stay, \(item.splitCount) would move to a new person."
    }
    static let skip = "Skip"
    static let skipHelp = "Hide this suggestion without deciding. Nothing is changed."
    static let notSameHelp = "These are two different people. FileID won't suggest merging them again."
    static let mergeHelp = "Combine both into one person. You can undo this right after."
    static let splitHelp = "Move the second group of faces to a new person. You can undo this right after."
    static func done(_ resolution: String) -> String {
        switch resolution {
        case "merge": return "Merged."
        case "notSamePerson": return "Saved: not the same person."
        case "split": return "Split into two people."
        default: return "Suggestion hidden."
        }
    }
}

enum FaceQueueKind: String, CaseIterable, Identifiable, Sendable {
    case faces, merges, splits
    var id: String { rawValue }
    var title: String {
        switch self {
        case .faces: return "Faces"
        case .merges: return "Merges"
        case .splits: return "Splits"
        }
    }
}

@MainActor
@Observable
final class FaceQueueModel {
    typealias Requester = FaceReviewModel.Requester

    let kind: FaceQueueKind
    private let request: Requester
    private(set) var merges: [FaceMergeSuggestion] = []
    private(set) var splits: [FaceSplitSuggestion] = []
    private(set) var total = 0
    private(set) var loaded = false
    private(set) var busy = false
    private(set) var message: String?
    private(set) var notice: String?
    private(set) var undoOperationID: String?
    private(set) var changes = 0

    init(kind: FaceQueueKind, request: @escaping Requester) {
        self.kind = kind
        self.request = request
    }

    convenience init(engine: EngineClient, kind: FaceQueueKind) {
        self.init(kind: kind) { command in await engine.faceReview(command) }
    }

    var isEmpty: Bool { merges.isEmpty && splits.isEmpty }

    func reload() async {
        let queue = kind.rawValue
        guard let page = await request({ .listFaceReview(requestID: $0, personID: nil, offset: 0, limit: 100, queue: queue) }) else {
            message = FaceReviewText.unreachable
            return
        }
        loaded = true
        guard page.status == "ok" else { message = page.message; return }
        merges = page.merges ?? []
        splits = page.splits ?? []
        total = page.total
    }

    func resolve(_ item: FaceMergeSuggestion, _ resolution: String) async {
        let keep = item.personID, other = item.otherPersonID
        await send(resolution) { .resolveFaceReview(requestID: $0, faceID: nil, personID: keep, resolution: resolution, otherPersonID: other) }
    }

    func resolve(_ item: FaceSplitSuggestion, _ resolution: String) async {
        let person = item.personID
        await send(resolution) { .resolveFaceReview(requestID: $0, faceID: nil, personID: person, resolution: resolution) }
    }

    private func send(_ resolution: String, _ command: @escaping (String) -> IPCCommand.Payload) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        guard let page = await request(command) else { message = FaceReviewText.unreachable; return }
        guard page.status == "ok" else { message = page.message ?? FaceReviewText.unreachable; return }
        message = nil
        notice = FaceQueueText.done(resolution)
        undoOperationID = page.operationID
        changes += 1
        await reload()
    }

    func undo() async {
        guard !busy, let operation = undoOperationID else { return }
        busy = true
        let page = await request({ .undoFaceReview(requestID: $0, operationID: operation) })
        busy = false
        guard let page else { message = FaceReviewText.unreachable; return }
        undoOperationID = nil
        notice = nil
        message = page.status == "ok" ? nil : page.message
        changes += 1
        await reload()
    }
}

enum FaceRefreshText {
    static let jobID = "face-refresh"
    static let update = "Update faces"
    static let updateLabel = "Update faces for the current face model"
    static func needs(_ count: Int) -> String { "Faces need updating (\(count))" }
    static let why = "These faces were saved by an earlier version of face matching. Updating them keeps people grouped correctly. It runs in the background."
    static let extrasHelp = "Missing: the photo was moved or deleted. Offline: the photo is on a drive that isn't connected. Failed: the photo changed or its face couldn't be read."
    static func explanation(_ status: FaceRefreshStatus) -> String? {
        var parts: [String] = []
        if status.outdated > 0 { parts.append(why) }
        if status.missing + status.offline + status.failed > 0 { parts.append(extrasHelp) }
        return parts.isEmpty ? nil : parts.joined(separator: " ")
    }
    static func extras(_ status: FaceRefreshStatus) -> String {
        [(status.missing, "missing"), (status.offline, "offline"), (status.failed, "failed")]
            .filter { $0.0 > 0 }.map { " · \($0.0) \($0.1)" }.joined()
    }
    static func line(_ status: FaceRefreshStatus) -> String? {
        switch status.state {
        case "running", "queued":
            return "Updating faces: \(Int(((status.progress ?? 0) * 100).rounded(.down)))%" + extras(status)
        case "paused":
            return "Updating faces: \(Int(((status.progress ?? 0) * 100).rounded(.down)))% (paused)" + extras(status)
        default:
            if status.outdated > 0 { return needs(status.outdated) }
            let tail = extras(status)
            return tail.isEmpty ? nil : "Faces updated" + tail
        }
    }
}

@MainActor
@Observable
final class FaceRefreshModel {
    typealias Requester = FaceReviewModel.Requester
    typealias CatalogSender = @MainActor (CatalogRequest) async -> CatalogResponse?

    private let request: Requester
    private let catalog: CatalogSender
    private(set) var status: FaceRefreshStatus?
    private(set) var message: String?
    private(set) var busy = false

    init(request: @escaping Requester, catalog: @escaping CatalogSender) {
        self.request = request
        self.catalog = catalog
    }

    convenience init(engine: EngineClient) {
        self.init(request: { command in await engine.faceReview(command) },
                  catalog: { request in await TakeRequests.send(engine, request, timeout: 15) })
    }

    var line: String? { message ?? status.flatMap(FaceRefreshText.line) }
    var explanation: String? { message == nil ? status.flatMap(FaceRefreshText.explanation) : nil }
    var isRunning: Bool { status?.state == "running" || status?.state == "queued" }
    var isPaused: Bool { status?.state == "paused" }
    var canStart: Bool { !isRunning && !isPaused && line != nil && !(status?.model.isEmpty ?? true) }

    func reload() async {
        guard let page = await request({ .listFaceReview(requestID: $0, personID: nil, offset: nil, limit: nil, queue: "refresh") }),
              page.status == "ok" else { return }
        status = page.refresh
    }

    func start() async { await run("enqueueFaceRefresh", jobID: nil) }
    func pause() async { await run("pauseJob", jobID: FaceRefreshText.jobID) }
    func resume() async { await run("resumeJob", jobID: FaceRefreshText.jobID) }
    func cancel() async { await run("cancelJob", jobID: FaceRefreshText.jobID) }

    private func run(_ action: String, jobID: String?) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        let response = await catalog(CatalogRequest(requestID: UUID().uuidString, action: action, jobID: jobID))
        message = response?.status == "error" ? response?.message : nil
        await reload()
    }
}
