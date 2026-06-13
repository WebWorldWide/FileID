// Butler semantic-classify parity tests — mirror the Windows Rust unit tests
// in restructure_semantic.rs so the Swift port behaves identically.
import Testing
import Foundation
@testable import FileIDEngine

@Suite("RestructureSemantic")
struct RestructureSemanticTests {

    private func unit(_ v: [Float]) -> [Float] {
        var n: Float = 0
        for x in v { n += x * x }
        n = n.squareRoot()
        return n < 1e-8 ? v : v.map { $0 / n }
    }

    private func file(_ id: Int64, _ path: String, _ clip: [Float], _ tags: [String])
        -> RestructureSemantic.SemanticFile {
        RestructureSemantic.SemanticFile(
            fileID: id, source: path, clip: unit(clip), tags: tags, timeUnix: 0)
    }

    @Test("Distinctive naming drops ubiquitous tags")
    func distinctiveNaming() {
        // "photo" tags every file (idf → 0, dropped); rarer tags name groups.
        var files: [RestructureSemantic.SemanticFile] = []
        for i in 0..<6 { files.append(file(Int64(i), "a/t\(i).jpg", [1, 0, 0], ["photo", "tree"])) }
        for i in 0..<4 {
            files.append(file(Int64(100 + i), "a/s\(i).jpg", [0, 1, 0], ["photo", "sunset", "beach"]))
        }
        let cats = Set(
            RestructureSemantic.classify(files: files, prototypes: [], libraryRoot: "/lib")
                .map { $0.category })
        #expect(cats.contains { $0.contains("Beach") || $0.contains("Sunset") })
        #expect(!cats.contains("Photo"))
    }

    @Test("Tight match to an existing folder auto-files with a reason")
    func tightFolderMatch() {
        let files = (0..<5).map { file(Int64($0), "inbox/d\($0).jpg", [1, 0, 0], ["dog"]) }
        let protos = [RestructureSemantic.FolderPrototype(path: "/lib/Dogs", centroid: unit([1, 0, 0]))]
        let moves = RestructureSemantic.classify(files: files, prototypes: protos, libraryRoot: "/lib")
        #expect(!moves.isEmpty)
        #expect(moves.allSatisfy { $0.confidence == .auto })
        #expect(moves.allSatisfy { $0.reason.contains("Dogs") })
    }

    @Test("Two distinct content groups get two distinct categories")
    func twoGroupsSeparate() {
        var files: [RestructureSemantic.SemanticFile] = []
        for i in 0..<6 { files.append(file(Int64(i), "src/dog\(i).jpg", [1, 0, 0, 0], ["dog", "park"])) }
        for i in 0..<6 {
            files.append(file(Int64(100 + i), "src/boat\(i).jpg", [0, 1, 0, 0], ["boat", "lake"]))
        }
        let cats = Set(
            RestructureSemantic.classify(files: files, prototypes: [], libraryRoot: "/lib")
                .map { $0.category })
        #expect(cats.count == 2)
    }

    /// F-C3-013/014: two distinct content clusters whose distinctive tags differ
    /// ONLY in characters componentSafe maps to "_" ("16:9" vs "16/9" → "16_9")
    /// must back DISTINCT physical directories (sanitize + dedup in the sanitized
    /// namespace), not collapse into one — and the numeric-suffix loop must
    /// terminate. Mirrors the Windows
    /// `sanitization_colliding_group_names_get_distinct_folders` test.
    @Test("Sanitization-colliding group names get distinct folders")
    func sanitizationCollidingGroups() {
        var files: [RestructureSemantic.SemanticFile] = []
        for i in 0..<6 { files.append(file(Int64(i), "a/r\(i).jpg", [1, 0, 0, 0], ["16:9"])) }
        for i in 0..<6 { files.append(file(Int64(100 + i), "a/s\(i).jpg", [0, 1, 0, 0], ["16/9"])) }
        let moves = RestructureSemantic.classify(files: files, prototypes: [], libraryRoot: "/lib")
        #expect(moves.count == 12)
        // destinationDir is the (sanitized) group folder; two colliding pretty
        // names must resolve to two distinct directories.
        let dirs = Set(moves.map { $0.destinationDir })
        #expect(dirs.count == 2)
        // Every folder must be sanitized: no separator survives in the new
        // group's last path component.
        #expect(dirs.allSatisfy { !($0 as NSString).lastPathComponent.contains("/") })
        #expect(dirs.allSatisfy { !($0 as NSString).lastPathComponent.contains(":") })
    }

    /// F-C3-015: a prototype that matches strongly but lives OUTSIDE libraryRoot
    /// is not a valid routing target (the apply layer would reject a move that
    /// canonicalizes outside root); the cluster falls through to a new in-root
    /// group instead.
    @Test("A prototype outside libraryRoot is not a routing target")
    func prototypeOutsideRootIgnored() {
        let files = (0..<5).map { file(Int64($0), "/lib/inbox/d\($0).jpg", [1, 0, 0], ["dog"]) }
        let protos = [RestructureSemantic.FolderPrototype(path: "/other/Dogs", centroid: unit([1, 0, 0]))]
        let moves = RestructureSemantic.classify(files: files, prototypes: protos, libraryRoot: "/lib")
        #expect(!moves.isEmpty)
        #expect(moves.allSatisfy { !$0.destinationDir.hasPrefix("/other") })
        #expect(moves.allSatisfy { RestructureSemantic.pathContained($0.destinationDir, in: "/lib") })
    }
}
