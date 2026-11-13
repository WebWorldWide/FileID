// Related-take proposals from the library-wide takeGrouping job: the relation
// chip, plain-language evidence and the job requests. Lockstep with WinUI
// TakeProposalPolicy and GTK takes_model.
import Foundation
import FileIDShared

enum TakeProposals {
    static let jobKind = "takeGrouping"

    enum Tone: Equatable { case gold, info, ai }

    static func relationLabel(_ relation: String?) -> String? {
        switch relation {
        case "repeatedAttempts": "Repeated takes"
        case "alternateViews": "Other angles"
        case "uncertain": "Not sure"
        default: nil
        }
    }

    static func relationTone(_ relation: String?) -> Tone {
        switch relation {
        case "repeatedAttempts": .gold
        case "alternateViews": .info
        default: .ai
        }
    }

    static func relationHelp(_ relation: String?) -> String? {
        switch relation {
        case "repeatedAttempts": "One camera took these one after another, so they are separate takes of the same shot."
        case "alternateViews": "Different cameras caught the same moment, so these are other angles or copies rather than separate takes."
        case "uncertain": "Camera details are missing or mixed, so FileID can't tell separate takes from other angles."
        default: nil
        }
    }

    static func evidenceSentence(_ code: String) -> String? {
        switch code {
        case "captureSession": "Taken close together."
        case "visualSimilarity": "They look alike."
        case "sharedSubject": "The same people are in every try."
        case "sameCamera": "One camera took them all."
        case "simultaneousCameras": "Different cameras took them at the same moment."
        case "mixedCameras": "They come from a mix of cameras."
        case "cameraDetailsMissing": "Some have no camera details."
        default: nil
        }
    }

    static func evidenceText(_ codes: [String]?) -> String {
        (codes ?? []).compactMap(evidenceSentence).joined(separator: " ")
    }

    static func job(_ jobs: [CatalogJob]) -> CatalogJob? { jobs.first { $0.kind == jobKind } }

    /// Find related tries is offered unless a run is already queued or running.
    static func canFind(_ job: CatalogJob?) -> Bool { !["queued", "running"].contains(job?.state ?? "") }

    /// True when the job has just finished a run, so the proposals should be read again.
    static func finished(previous: CatalogJob?, current: CatalogJob?) -> Bool {
        guard let current, current.state == "completed" else { return false }
        return previous?.state != "completed" || previous?.updatedAt != current.updatedAt
    }

    static func enqueueRequest(id: String) -> CatalogRequest {
        CatalogRequest(requestID: id, action: "enqueueTakeGrouping")
    }

    static func proposalsRequest(id: String) -> CatalogRequest {
        CatalogRequest(requestID: id, action: "takeGroupProposals", limit: 100)
    }

    static func accessibilityLabel(_ group: CatalogTakeAutoGroup, date: String) -> String {
        let noun = group.kind == "videos" ? "clips" : "photos"
        let relation = relationLabel(group.relation).map { ", \($0)" } ?? ""
        let skipped = group.status == "skipped" ? ", skipped" : ""
        let evidence = evidenceText(group.evidence)
        return "\(group.shots.count) \(noun)\(relation), \(date)\(skipped)" + (evidence.isEmpty ? "" : ". " + evidence)
    }

    /// The members to put in a new group when the person saves a proposal: one per try.
    static func saveHits(_ group: CatalogTakeAutoGroup) -> [CatalogHit] {
        let kind = group.kind == "videos" ? "video" : "image"
        return group.shots.map { CatalogHit(fileID: $0.primaryFileID, path: $0.path, kind: kind, text: "") }
    }
}
