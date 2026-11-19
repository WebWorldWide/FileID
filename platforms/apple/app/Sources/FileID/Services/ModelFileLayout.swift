import Foundation

/// The engine keeps some models under a second folder layout (same artifact, same SHA-256) and on macOS accepts either; Settings must call a model installed under either too.
enum ModelFileLayout {
    private static let pairs: [(swift: String, rust: String)] = [
        ("face_recognition_sface_2021dec.onnx", "sface/face_recognition_sface_2021dec.onnx"),
        ("mobileclip_image/clip_vitb32_image.onnx", "mobileclip/mobileclip_s2_image.onnx"),
    ]

    static func alternate(of file: URL, in root: URL) -> URL? {
        let prefix = root.standardizedFileURL.path + "/"
        let path = file.standardizedFileURL.path
        guard path.hasPrefix(prefix) else { return nil }
        let relative = String(path.dropFirst(prefix.count))
        for pair in pairs {
            if relative == pair.swift { return url(pair.rust, in: root) }
            if relative == pair.rust { return url(pair.swift, in: root) }
        }
        return nil
    }

    /// The file itself when present, else its other-layout copy.
    static func installedFile(_ file: URL, in root: URL = AppSupportPath.models) -> URL? {
        if FileManager.default.fileExists(atPath: file.path) { return file }
        guard let other = alternate(of: file, in: root) else { return nil }
        var isDirectory: ObjCBool = false
        return FileManager.default.fileExists(atPath: other.path, isDirectory: &isDirectory) && !isDirectory.boolValue ? other : nil
    }

    private static func url(_ relative: String, in root: URL) -> URL {
        relative.split(separator: "/").reduce(root) { $0.appendingPathComponent(String($1)) }
    }
}
