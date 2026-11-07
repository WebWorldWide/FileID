import Foundation
import Observation
import FileIDShared

enum FaceReviewText {
    static let title = "Review suggestions"
    static let empty = "No suggestions to review."
    static let undo = "Undo"
    static let undoLabel = "Undo the last review"
    static let unreachable = "FileID couldn't reach the engine. Try again in a moment."

    static func name(_ value: String?) -> String {
        let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed.isEmpty ? "an unnamed person" : trimmed
    }

    static func percent(_ score: Double) -> Int { Int((score * 100).rounded()) }

    static func reason(_ item: FaceReviewItem) -> String {
        let person = name(item.personName)
        switch item.reason {
        case "uncertain":
            return "Might be \(person), but the match is weak (\(percent(item.score))%)."
        case "ambiguous":
            guard item.runnerUpName != nil || item.runnerUpPersonID != nil else {
                return "Could be \(person), but it's a close call."
            }
            return "Could be \(person) or \(name(item.runnerUpName)); both look about as close."
        default:
            return "Looks like \(person) (\(percent(item.score))% match)."
        }
    }

    static func fileName(_ item: FaceReviewItem) -> String { (item.path as NSString).lastPathComponent }
    static func faceLabel(_ item: FaceReviewItem) -> String { "Face in \(fileName(item))" }
    static func acceptLabel(_ item: FaceReviewItem) -> String { "Accept: this is \(name(item.personName))" }
    static func rejectLabel(_ item: FaceReviewItem) -> String { "Not \(name(item.personName))" }
    static let ignoreLabel = "Ignore this face"
    static func rowLabel(_ item: FaceReviewItem) -> String { "\(reason(item)) In \(fileName(item))." }
    static func header(_ count: Int) -> String { "\(title), \(count) waiting" }
}

enum FaceReviewResolution: String, CaseIterable, Sendable {
    case accept, reject, ignore
}

@MainActor
@Observable
final class FaceReviewModel {
    typealias Requester = @MainActor (_ command: (String) -> IPCCommand.Payload) async -> FaceReviewPage?

    static let pageSize = 50

    let personID: Int64?
    private let request: Requester
    private(set) var items: [FaceReviewItem] = []
    private(set) var total = 0
    private(set) var loaded = false
    private(set) var busy = false
    private(set) var message: String?
    private(set) var undoOperationID: String?
    private(set) var changes = 0

    init(personID: Int64? = nil, request: @escaping Requester) {
        self.personID = personID
        self.request = request
    }

    convenience init(engine: EngineClient, personID: Int64? = nil) {
        self.init(personID: personID) { command in await engine.faceReview(command) }
    }

    var hasMore: Bool { items.count < total }
    var isVisible: Bool { total > 0 || undoOperationID != nil || message != nil }

    func reload() async {
        let person = personID
        let limit = Self.pageSize
        guard let page = await request({ .listFaceReview(requestID: $0, personID: person, offset: 0, limit: limit) }) else {
            message = FaceReviewText.unreachable
            return
        }
        loaded = true
        guard page.status == "ok" else { message = page.message; return }
        items = page.items
        total = page.total
    }

    func loadMore() async {
        let person = personID
        let offset = items.count
        let limit = Self.pageSize
        guard let page = await request({ .listFaceReview(requestID: $0, personID: person, offset: offset, limit: limit) }),
              page.status == "ok" else { return }
        let known = Set(items.map(\.faceID))
        items.append(contentsOf: page.items.filter { !known.contains($0.faceID) })
        total = page.total
    }

    func resolve(_ item: FaceReviewItem, _ resolution: FaceReviewResolution) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        let face = item.faceID, person = item.personID, value = resolution.rawValue
        guard let page = await request({ .resolveFaceReview(requestID: $0, faceID: face, personID: person, resolution: value) }) else {
            message = FaceReviewText.unreachable
            return
        }
        guard page.status == "ok" else {
            message = page.message ?? FaceReviewText.unreachable
            return
        }
        items.removeAll { $0.faceID == face }
        total = max(0, total - 1)
        message = nil
        undoOperationID = page.operationID
        changes += 1
    }

    func undo() async {
        guard !busy, let operation = undoOperationID else { return }
        busy = true
        let page = await request({ .undoFaceReview(requestID: $0, operationID: operation) })
        busy = false
        guard let page else { message = FaceReviewText.unreachable; return }
        undoOperationID = nil
        message = page.status == "ok" ? nil : page.message
        changes += 1
        await reload()
    }
}
