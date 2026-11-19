import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Video tool requests")
struct MediaToolRequestPolicyTests {
    private func capability(_ id: String, available: Bool = true) -> ToolCapability {
        ToolCapability(id: id, available: available, inputFormats: ["mp4", "mov", "m4v"], outputFormats: ["mp4"], detail: "")
    }

    @Test func subPickerOffersOnlyToolsTheEngineReportsAvailable() {
        #expect(MediaToolChoice.available([]).map(\.0) == ["export"])
        let all = ["trim", "remux", "audio", "proxy", "rotate"].map { capability($0) }
        #expect(MediaToolChoice.available(all).map(\.0) == ["export", "trim", "remux", "audio", "proxy", "rotate"])
        #expect(MediaToolChoice.available(all).map(\.1) == ["Export", "Trim", "Copy without re-compressing", "Audio", "Small preview copy", "Rotate"])
        let partial = [capability("trim"), capability("remux", available: false), capability("rotate")]
        #expect(MediaToolChoice.available(partial).map(\.0) == ["export", "trim", "rotate"])
    }

    @MainActor
    @Test func exportKeepsTheVideoRecipeAndCarriesNoMediaFields() {
        let session = ToolsSession()
        session.kind = "video"; session.format = "mp4"; session.maxDimension = 1920
        session.videoAspectRatio = "9:16"; session.videoFraming = "auto"
        session.media.trimStart = 1; session.media.trimEnd = 2; session.media.rotation = 180
        let recipe = session.recipe
        #expect(recipe.kind == "video" && recipe.format == "mp4" && recipe.maxDimension == 1920)
        #expect(recipe.videoAspectRatio == "9:16" && recipe.videoFraming == "auto")
        #expect(recipe.trimStartSeconds == nil && recipe.trimEndSeconds == nil && recipe.trimMode == nil && recipe.rotationDegrees == nil)
        session.kind = "photo"; session.format = "png"
        session.media.tool = "rotate"
        #expect(session.recipe.kind == "photo" && session.recipe.rotationDegrees == nil)
    }

    @MainActor
    @Test func mediaToolsSendOnlyTheFieldsTheirRecipeTakes() {
        let session = ToolsSession()
        session.kind = "video"; session.allowUpscale = true; session.videoAspectRatio = "1:1"; session.videoStabilization = true

        session.media.adopt(tool: "trim", sources: ["/v/Clip.mov"])
        session.media.trimStart = 1.25; session.media.trimEnd = 4.5
        var recipe = session.recipe
        #expect(recipe == ToolRecipe(kind: "trim", format: "mov", maxDimension: 1920, trimStartSeconds: 1.25, trimEndSeconds: 4.5, trimMode: "keyframe"))
        session.media.trimMode = "exact"
        #expect(session.recipe.format == "mp4")
        #expect(session.media.disabledContainers(sources: ["/v/Clip.mov"]) == ["mov"])

        session.media.adopt(tool: "audio", sources: ["/v/Clip.mov"])
        #expect(session.recipe == ToolRecipe(kind: "audio", format: "m4a", maxDimension: 1920))

        session.media.adopt(tool: "proxy", sources: ["/v/Clip.mov"])
        session.media.proxySide = 960
        #expect(session.recipe == ToolRecipe(kind: "proxy", format: "mp4", maxDimension: 960))

        session.media.adopt(tool: "rotate", sources: ["/v/Clip.mp4"])
        for (degrees, angle) in [(90, 90.0), (180, 180.0), (270, -90.0)] {
            session.media.rotation = degrees
            recipe = session.recipe
            #expect(recipe == ToolRecipe(kind: "rotate", format: "mp4", maxDimension: 1920, rotationDegrees: degrees))
            #expect(session.media.previewAngle == angle)
        }
        #expect(MediaToolChoice.rotations.map(\.1) == ["90° right", "180°", "90° left"])
        session.media.adopt(tool: "export", sources: [])
        #expect(session.media.previewAngle == 0 && session.recipe.kind == "video")
    }

    @Test func remuxDisablesTheContainerTheSourcesAlreadyHave() {
        var choice = MediaToolChoice()
        choice.container = "mov"
        choice.adopt(tool: "remux", sources: ["/v/Clip.mov"])
        #expect(choice.disabledContainers(sources: ["/v/Clip.mov"]) == ["mov"])
        #expect(choice.recipe == ToolRecipe(kind: "remux", format: "mp4", maxDimension: 1920))
        choice.settleContainer(sources: ["/v/Phone.M4V"])
        #expect(choice.disabledContainers(sources: ["/v/Phone.M4V"]) == ["mp4"])
        #expect(choice.recipe?.format == "mov")
        let mixed = ["/v/A.mp4", "/v/B.mov"]
        #expect(choice.disabledContainers(sources: mixed) == ["mp4", "mov"])
        choice.settleContainer(sources: mixed)
        #expect(choice.recipe?.format == "mov", "with both containers taken the choice stays and the engine's refusal is shown")
        #expect(MediaToolChoice.container(ofPath: "/v/notes.txt") == nil)
    }
    @MainActor
    @Test func theNothingMatchedLineQuotesTheSearchThatWasSentAndOnlyWhenItSucceeded() {
        let session = ToolsSession()
        session.query = "IMG_41"
        session.searchedQuery = "IMG_41"
        session.query = "something typed afterwards"
        session.recordSearchReply(status: "ok")
        #expect(session.pickFilesNote.contains("“IMG_41”"))
        session.recordSearchReply(status: "error")
        #expect(session.pickFilesNote.hasPrefix("Search for files by name"))
    }
    @MainActor
    @Test func anEmptySearchSaysNothingMatchedInsteadOfRepeatingTheIntro() {
        let session = ToolsSession()
        #expect(session.pickFilesNote.hasPrefix("Search for files by name"))
        session.answeredQuery = "IMG_41"
        #expect(session.pickFilesNote == "Nothing in the library matches “IMG_41”. Try part of a file name or a few words about what's in it.")
        session.allHits = [CatalogHit(fileID: 1, path: "/v/a.mov", kind: "video", text: "")]
        session.applyKindFilter()
        #expect(session.hits.isEmpty)
        #expect(session.pickFilesNote.hasPrefix("That search found 1 file, but no photos."))
    }
    @Test func aVideoSearchKeepsMp4AndMovHitsInVideoMode() {
        let session = ToolsSession()
        session.allHits = [
            CatalogHit(fileID: 1, path: "/v/C0029.mp4", kind: "video", text: ""),
            CatalogHit(fileID: 2, path: "/v/C0030.MOV", kind: "video", text: ""),
            CatalogHit(fileID: 3, path: "/v/still.jpg", kind: "image", text: ""),
        ]
        session.kind = "video"
        session.applyKindFilter()
        #expect(session.hits.map(\.fileID) == [1, 2])
    }

    @Test func aPreviewSaysWhatExportWillDoInPlainWords() {
        let photoCaveat = " Photo copies leave out details such as location and use standard 8-bit colors."
        func plain(_ action: String = "preview", _ status: String = "ok", outputs: Int, kind: String?, _ message: String) -> String {
            ToolsSession.plainMessage(action: action, status: status, outputs: outputs, recipeKind: kind, message: message)
        }
        #expect(plain(outputs: 2, kind: "photo", "Creates 2 new file(s).") == "Press Export to make 2 new files. Your originals stay as they are." + photoCaveat)
        #expect(plain(outputs: 1, kind: "photo", "Anything the engine words differently") == "Press Export to make 1 new file. Your original stays as it is." + photoCaveat)
        #expect(plain(outputs: 1, kind: "video", "Creates 1 new file(s). Photo outputs normalize to 8-bit SDR.") == "Press Export to make 1 new file. Your original stays as it is.")
        #expect(plain(outputs: 1, kind: nil, "Creates 1 new file(s).") == "Press Export to make 1 new file. Your original stays as it is.")
        #expect(plain("preview", "error", outputs: 0, kind: "photo", "No such file") == "No such file")
        #expect(plain("execute", "ok", outputs: 1, kind: "photo", "Done") == "Done")
    }
}
