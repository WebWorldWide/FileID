import Foundation

public enum ModelLicenseAcceptance {
    public static let reviewedAt = "2026-07-16"

    public static func isAccepted(for kind: AIModelKind) -> Bool {
        guard kind.licenseTermsURL != nil else { return true }
        guard let marker = try? markerURL(for: kind) else { return false }
        guard let values = try? marker.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey]),
              values.isRegularFile == true,
              values.isSymbolicLink != true,
              let contents = try? String(contentsOf: marker, encoding: .utf8) else {
            return false
        }
        return contents == markerContents(for: kind)
    }

    public static func recordAcceptance(for kind: AIModelKind) throws {
        guard kind.licenseTermsURL != nil else { return }
        let marker = try markerURL(for: kind)
        let directory = marker.deletingLastPathComponent()
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        try Data(markerContents(for: kind).utf8).write(to: marker, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: marker.path)
        guard isAccepted(for: kind) else {
            throw CocoaError(.fileWriteUnknown)
        }
    }

    static func markerURL(for kind: AIModelKind, root: URL? = nil) throws -> URL {
        let base: URL
        if let root {
            base = root
        } else {
            guard let applicationSupport = FileManager.default.urls(
                for: .applicationSupportDirectory,
                in: .userDomainMask
            ).first else {
                throw CocoaError(.fileNoSuchFile)
            }
            base = applicationSupport.appending(component: "FileID", directoryHint: .isDirectory)
        }
        return base
            .appending(component: "LicenseAcceptances", directoryHint: .isDirectory)
            .appending(component: "\(kind.licensePolicyKey)-\(reviewedAt).accepted")
    }

    static func isAccepted(for kind: AIModelKind, root: URL) -> Bool {
        guard kind.licenseTermsURL != nil else { return true }
        guard let marker = try? markerURL(for: kind, root: root),
              let contents = try? String(contentsOf: marker, encoding: .utf8) else {
            return false
        }
        return contents == markerContents(for: kind)
    }

    static func recordAcceptance(for kind: AIModelKind, root: URL) throws {
        guard kind.licenseTermsURL != nil else { return }
        let marker = try markerURL(for: kind, root: root)
        try FileManager.default.createDirectory(
            at: marker.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try Data(markerContents(for: kind).utf8).write(to: marker, options: .atomic)
        guard isAccepted(for: kind, root: root) else {
            throw CocoaError(.fileWriteUnknown)
        }
    }

    private static func markerContents(for kind: AIModelKind) -> String {
        "policy=\(kind.licensePolicyKey)\nreviewedAt=\(reviewedAt)\nterms=\(kind.licenseTermsURL?.absoluteString ?? "")\n"
    }
}
