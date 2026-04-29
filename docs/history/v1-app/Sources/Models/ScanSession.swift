import Foundation
import SwiftData

@Model
final class ScanSession {
    @Attribute(.unique) var id: UUID
    var folderPath:    String
    var startedAt:     Date
    var completedAt:   Date?
    var totalFiles:    Int
    var processedFiles: Int

    var isComplete: Bool { completedAt != nil }

    init(folderPath: String) {
        self.id             = UUID()
        self.folderPath     = folderPath
        self.startedAt      = Date()
        self.completedAt    = nil
        self.totalFiles     = 0
        self.processedFiles = 0
    }
}
