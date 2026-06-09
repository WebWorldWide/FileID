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
}
