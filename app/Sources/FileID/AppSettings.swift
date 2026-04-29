// Centralized @AppStorage keys + defaults.
import Foundation

enum AppSettings {
    /// People-tab primary clustering: true = VLM-driven (more accurate),
    /// false = L2-only (faster, more false splits).
    static let useAIFaceClusteringKey = "ai.fileid.faceClustering.useAI"
    static let useAIFaceClusteringDefault: Bool = true
}
