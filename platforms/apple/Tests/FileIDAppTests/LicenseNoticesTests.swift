import Foundation
import Testing
@testable import FileID

@Suite("Open-source acknowledgements")
struct LicenseNoticesTests {
    private func makeBundle(files: [String]) throws -> (Bundle, URL) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("licenses-\(UUID().uuidString)")
        let resources = root.appendingPathComponent("Test.bundle/Contents/Resources")
        try FileManager.default.createDirectory(at: resources, withIntermediateDirectories: true)
        try Data("<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>test.licenses</string></dict></plist>".utf8)
            .write(to: root.appendingPathComponent("Test.bundle/Contents/Info.plist"))
        for file in files { try Data("text".utf8).write(to: resources.appendingPathComponent(file)) }
        return (try #require(Bundle(url: root.appendingPathComponent("Test.bundle"))), root)
    }

    @Test("the packaged app's notices and licence files are found by name")
    func finds() throws {
        let (bundle, root) = try makeBundle(files: ["THIRD_PARTY_NOTICES.txt", "LICENSE.txt"])
        defer { try? FileManager.default.removeItem(at: root) }
        #expect(LicenseNotices.noticesURL(in: bundle)?.lastPathComponent == "THIRD_PARTY_NOTICES.txt")
        #expect(LicenseNotices.licenseURL(in: bundle)?.lastPathComponent == "LICENSE.txt")
    }

    @Test("a bundle without the files gives no link rather than a broken one")
    func missing() throws {
        let (bundle, root) = try makeBundle(files: [])
        defer { try? FileManager.default.removeItem(at: root) }
        #expect(LicenseNotices.noticesURL(in: bundle) == nil)
        #expect(LicenseNotices.licenseURL(in: bundle) == nil)
    }
}
