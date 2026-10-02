import Foundation
import Testing
@testable import FileIDShared

@Suite("Read-only example locations")
struct ReadOnlyLocationsTests {
    @Test func protectsAliasesAndUncreatedDestinations() throws {
        let base = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let protected = base.appendingPathComponent("example")
        try FileManager.default.createDirectory(at: protected, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let alias = base.appendingPathComponent("alias")
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: protected)
        for url in [protected, protected.appendingPathComponent("new/out.mp4"), alias.appendingPathComponent("cache/db.sqlite")] {
            #expect(throws: ReadOnlyLocations.ProtectionError.self) {
                try ReadOnlyLocations.requireWritable(url, protectedRoots: [protected])
            }
        }
        try ReadOnlyLocations.requireWritable(base.appendingPathComponent("example-other/out.mp4"), protectedRoots: [protected])
    }

    @Test func defaultProtectsAdlonWithoutOpeningIt() {
        #expect(throws: ReadOnlyLocations.ProtectionError.self) {
            try ReadOnlyLocations.requireWritable(URL(fileURLWithPath: "/Volumes/Adlon/new/export.mov"))
        }
    }

    @Test func rejectsDanglingFileAndDirectoryAliases() throws {
        let base = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let protected = base.appendingPathComponent("example", isDirectory: true)
        try FileManager.default.createDirectory(at: protected, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let fileAlias = base.appendingPathComponent("output.png")
        let directoryAlias = base.appendingPathComponent("output-directory")
        try FileManager.default.createSymbolicLink(at: fileAlias, withDestinationURL: protected.appendingPathComponent("new.png"))
        try FileManager.default.createSymbolicLink(at: directoryAlias, withDestinationURL: protected.appendingPathComponent("new-directory"))
        for url in [fileAlias,directoryAlias.appendingPathComponent("new.png")] {
            #expect(throws: ReadOnlyLocations.UnsafeAliasError.self) { try ReadOnlyLocations.requireWritable(url, protectedRoots: [protected]) }
        }
        #expect(!FileManager.default.fileExists(atPath: protected.appendingPathComponent("new.png").path))
    }

    @Test func protectsManagedOriginalsAndAliases() throws {
        let base = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let bundle = base.appendingPathComponent("Movies.fcpbundle/Original Media", isDirectory: true)
        try FileManager.default.createDirectory(at: bundle, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: base) }
        let alias = base.appendingPathComponent("alias")
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: bundle)
        for url in [bundle.appendingPathComponent("clip.mov"), alias.appendingPathComponent("clip.mov"), base.appendingPathComponent("Final Cut Original Media/clip.mov")] {
            #expect(throws: ReadOnlyLocations.ManagedLibraryError.self) { try ReadOnlyLocations.requireSourceMutation(url) }
        }
        try ReadOnlyLocations.requireSourceMutation(base.appendingPathComponent("Exports/clip.mov"))
    }
}
