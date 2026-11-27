import Foundation

/// Every model size on screen goes through this, so one model reads the same in Settings, Welcome and the model sheets.
enum ModelSize {
    static func label(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    static func downloadBadge(_ bytes: Int64) -> String {
        "\(label(bytes)) download"
    }
}
