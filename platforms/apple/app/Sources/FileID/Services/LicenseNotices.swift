import Foundation

enum LicenseNotices {
    static let noticesName = "THIRD_PARTY_NOTICES"
    static let licenseName = "LICENSE"

    static func noticesURL(in bundle: Bundle = .main) -> URL? {
        bundle.url(forResource: noticesName, withExtension: "txt")
    }

    static func licenseURL(in bundle: Bundle = .main) -> URL? {
        bundle.url(forResource: licenseName, withExtension: "txt")
    }
}
