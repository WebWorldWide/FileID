import Foundation
import Testing
@testable import FileID

@Suite("Copy wording")
struct CopyWordingTests {
    private static var appSources: URL {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        return root.appendingPathComponent("platforms/apple/app/Sources/FileID")
    }

    private static func sourceLines() throws -> [(file: String, number: Int, text: String)] {
        let files = try #require(FileManager.default.enumerator(at: appSources, includingPropertiesForKeys: nil))
        var lines: [(String, Int, String)] = []
        for case let url as URL in files where url.pathExtension == "swift" {
            let text = try String(contentsOf: url, encoding: .utf8)
            for (index, line) in text.split(separator: "\n", omittingEmptySubsequences: false).enumerated() {
                lines.append((url.lastPathComponent, index + 1, String(line)))
            }
        }
        return lines
    }

    @Test func countsReadSingularAtOneAndGroupThousands() {
        #expect(Plural.noun(0, "file", "files") == "0 files")
        #expect(Plural.noun(1, "file", "files") == "1 file")
        #expect(Plural.noun(2, "person", "people") == "2 people")
        #expect(Plural.noun(1, "copy", "copies") == "1 copy")
        #expect(Plural.noun(Int64(1_200), "file", "files") == "1,200 files")
        #expect(FaceQueueText.faces(1) == "1 face")
        #expect(FaceQueueText.faces(3) == "3 faces")
    }

    @Test func aCountedNounInAStringGoesThroughThePluralHelper() throws {
        let counted = try NSRegularExpression(pattern: #"\\\([^"]*\)\s(files|faces|photos|people|takes|copies|results|cards)\b"#)
        var offenders: [String] = []
        for line in try Self.sourceLines() {
            let text = line.text.trimmingCharacters(in: .whitespaces)
            if text.hasPrefix("//") || line.text.contains("== 1") || line.text.contains("Plural.noun") || line.text.contains("WHERE") { continue }
            if counted.firstMatch(in: line.text, range: NSRange(line.text.startIndex..., in: line.text)) != nil {
                offenders.append("\(line.file):\(line.number)")
            }
        }
        #expect(offenders.isEmpty, "\(offenders)")
    }

    @Test func userCopyUsesCurlyApostrophesAndQuotes() throws {
        let straightApostrophe = try NSRegularExpression(pattern: #"[A-Za-z]'[A-Za-z]"#)
        var offenders: [String] = []
        for line in try Self.sourceLines() {
            let text = line.text.trimmingCharacters(in: .whitespaces)
            if text.hasPrefix("//") || text.hasPrefix("*") || text.hasPrefix("--") { continue }
            guard let quote = line.text.firstIndex(of: "\"") else { continue }
            if line.text.range(of: #"SELECT|INSERT|UPDATE|DELETE|FROM |WHERE|json_extract|LIKE |PRAGMA"#, options: .regularExpression) != nil { continue }
            let afterQuote = String(line.text[quote...])
            let code = afterQuote.components(separatedBy: "   // ").first ?? afterQuote
            if straightApostrophe.firstMatch(in: code, range: NSRange(code.startIndex..., in: code)) != nil {
                offenders.append("\(line.file):\(line.number)")
            }
            if code.contains("\\\"") && !code.contains("\\\\\"") { offenders.append("\(line.file):\(line.number) quote") }
        }
        #expect(offenders.isEmpty, "\(offenders)")
    }

    @Test func aWellOrganizedRootSaysWhyNothingMoves() {
        #expect(RestructureView.nothingToMoveBody(rootName: "Camera")
                == "Your folders inside “Camera” are already well organized. FileID doesn’t rename the folder you chose.")
        #expect(RestructureView.nothingToMoveBody(rootName: nil).hasPrefix("Your folders in it are already well organized."))
    }
}

@Suite("Chat scope wording")
struct ChatScopeWordingTests {
    @Test("documents, audio and unknown files are never called photos or videos")
    func onlyRealPhotosAndVideosGetThoseNames() {
        func label(_ paths: [String]) -> String? {
            ChatScope.selected(Array(1...Int64(paths.count)), kinds: paths.map(ChatScope.kind))?.label
        }
        #expect(label(["/a/one.pdf", "/a/two.docx"]) == "2 selected files")
        #expect(label(["/a/song.mp3"]) == "1 selected file")
        #expect(label(["/a/notes"]) == "1 selected file")
        #expect(label(["/a/p.JPG", "/a/q.heic"]) == "2 selected photos")
        #expect(label(["/a/m.mov", "/a/n.mp4"]) == "2 selected videos")
    }
}

@Suite("Organize read-only note")
struct RestructureReadOnlyNoteTests {
    @Test func theNoteNamesTheDriveAndHowManyMovesCannotRun() {
        let reason = ReadOnlyLocations.sentence(forDrive: "Adlon")
        let note = RestructureView.readOnlyMoveNote(reason: reason, moveCount: 55)
        #expect(note.hasPrefix(reason))
        #expect(note.contains("The 55 moves in this plan can’t run."))
        #expect(RestructureView.readOnlyMoveNote(reason: reason, moveCount: 1).contains("The 1 move in this plan"))
    }
}
