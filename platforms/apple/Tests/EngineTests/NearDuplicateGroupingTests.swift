import Foundation
import Testing
@testable import FileIDEngine

struct NearDuplicateGroupingTests {
    private func vector(_ angle: Float) -> [Float] {
        var v = [Float](repeating: 0, count: 8)
        v[0] = cos(angle)
        v[1] = sin(angle)
        return v
    }

    private func item(_ id: Int64, _ path: String, _ time: Double?, _ angle: Float?, phash: UInt64? = nil) -> NearDuplicateGrouping.Item {
        .init(id: id, path: path, time: time, phash: phash, embedding: angle.map(vector))
    }

    @Test func groupsABurstAndItsRawSidecarsButNotTheNextShot() {
        let items = [
            item(1, "/c/JPEGs/DSC001.JPG", 100, 0),
            item(2, "/c/RAWs/DSC001.ARW", 100, 0),
            item(3, "/c/JPEGs/DSC002.JPG", 101, 0.05),
            item(4, "/c/RAWs/DSC002.ARW", 101, 0.05),
            item(5, "/c/JPEGs/DSC003.JPG", 102, 1.2),
        ]
        let groups = NearDuplicateGrouping.groups(items)
        #expect(groups.map(\.members) == [[1, 2, 3, 4], [5]])
        #expect(groups[0].representative == 1 || groups[0].representative == 3)
    }

    @Test func aLaterExportOfTheRawJoinsItsShot() {
        let groups = NearDuplicateGrouping.groups([
            item(1, "/c/RAWs/DSC9.ARW", 100, 0),
            item(2, "/c/JPEGs/DSC9.JPG", 15_000, 0.3),
            item(3, "/c/JPEGs/DSC8.JPG", 15_000, 0),
        ])
        #expect(groups.map(\.members) == [[2, 1], [3]])
    }

    @Test func timeGapsAndMissingTimesNeverJoin() {
        let groups = NearDuplicateGrouping.groups([
            item(1, "/a/1.jpg", 10, 0), item(2, "/a/2.jpg", 13, 0),
            item(3, "/a/3.jpg", nil, 0), item(4, "/a/4.jpg", nil, 0),
        ])
        #expect(groups.allSatisfy { $0.members.count == 1 })
    }

    @Test func missingEmbeddingsKeepFilesSeparateExceptSidecarPairs() {
        let groups = NearDuplicateGrouping.groups([
            item(1, "/a/IMG_1.HEIC", 10, nil), item(2, "/a/IMG_1.DNG", 10, nil), item(3, "/a/IMG_2.HEIC", 10.5, nil),
        ])
        #expect(groups.map(\.members) == [[2, 1], [3]])
        #expect(groups[0].representative == 1)
    }

    @Test func driftFromTheFirstFrameSplitsALongPan() {
        let items = (0..<10).map { item(Int64($0), "/a/\($0).jpg", Double($0), Float($0) * 0.2) }
        let groups = NearDuplicateGrouping.groups(items)
        #expect(groups.count > 1)
        for group in groups {
            let angles = group.members.map { Float($0) * 0.2 }
            #expect(cos(angles.max()! - angles.min()!) >= NearDuplicateGrouping.minSeedCosine)
        }
    }

    @Test func sequenceSuffixesNumberShotsAndShareANumberAcrossSidecars() {
        #expect(NearDuplicateGrouping.sequenceSuffixes(paths: ["/j/A1.JPG", "/r/A1.ARW", "/j/A2.JPG"]) == [" 1", " 1", " 2"])
        #expect(NearDuplicateGrouping.sequenceSuffixes(paths: ["/j/A1.JPG", "/r/A1.ARW"]) == ["", ""])
        let many = (1...10).map { "/j/S\($0).jpg" }
        #expect(NearDuplicateGrouping.sequenceSuffixes(paths: many).first == " 01")
        #expect(NearDuplicateGrouping.sequenceSuffixes(paths: many).last == " 10")
    }

    @Test func appendingSequenceKeepsTheSixtyCharacterBudget() {
        let long = String(repeating: "Word ", count: 20)
        let named = DeepAnalyzeRunner.appendingSequence(long, " 07")
        #expect(named.count <= 60)
        #expect(named.hasSuffix(" 07"))
        #expect(DeepAnalyzeRunner.appendingSequence("Boy Swinging Bat", "") == "Boy Swinging Bat")
    }

    @Test func embeddingBlobDecodesLittleEndianFloats() {
        var data = Data()
        for value: Float in [1, -2.5] { withUnsafeBytes(of: value.bitPattern.littleEndian) { data.append(contentsOf: $0) } }
        #expect(NearDuplicateGrouping.decodeEmbedding(data) == [1, -2.5])
        #expect(NearDuplicateGrouping.decodeEmbedding(Data([1, 2, 3])) == nil)
    }
}

struct VideoSceneSelectionTests {
    private func frame(_ seconds: Double, _ angle: Float, change: Double = 0) -> VideoSceneSelection.Frame {
        var v = [Float](repeating: 0, count: 4)
        v[0] = cos(angle)
        v[1] = sin(angle)
        return .init(seconds: seconds, embedding: v, change: change)
    }

    @Test func cutsScenesOnEmbeddingJumpsAndPicksAMemberRepresentative() {
        let frames = (0..<6).map { frame(Double($0) * 0.5, 0) } + (6..<12).map { frame(Double($0) * 0.5, 1.5) }
        let scenes = VideoSceneSelection.scenes(frames, duration: 6)
        #expect(scenes.count == 2)
        #expect(scenes[0].start == 0 && scenes[0].end == 3)
        #expect(scenes[1].start == 3 && scenes[1].end == 6)
        #expect(scenes[0].frames.contains(scenes[0].representative))
        #expect(scenes[1].frames.contains(scenes[1].representative))
    }

    @Test func ignoresAFlashShorterThanTheMinimumScene() {
        let frames = [frame(0, 0), frame(0.5, 1.5), frame(1.0, 0), frame(1.5, 0), frame(2.0, 0)]
        #expect(VideoSceneSelection.scenes(frames, duration: 2.5).count <= 2)
        #expect(VideoSceneSelection.scenes([], duration: 2).isEmpty)
    }

    @Test func activityWindowsCoverOnlyMotionAndRespectFrameLimits() {
        var frames = (0..<40).map { frame(Double($0) * 0.5, 0) }
        for i in 10..<14 { frames[i] = frame(Double(i) * 0.5, 0, change: 0.05) }
        let windows = VideoSceneSelection.activityWindows(frames, duration: 20)
        #expect(windows.count == 1)
        #expect(windows[0].start >= 4 && windows[0].end <= 7.5)
        #expect((2...VideoSceneSelection.maxWindowFrames).contains(windows[0].times.count))
        #expect(VideoSceneSelection.activityWindows((0..<10).map { frame(Double($0), 0) }, duration: 10).isEmpty)
    }

    @Test func longActivitySplitsIntoBoundedWindows() {
        let frames = (0..<40).map { frame(Double($0) * 0.5, 0, change: 0.1) }
        let windows = VideoSceneSelection.activityWindows(frames, duration: 20)
        #expect(windows.count >= 5)
        #expect(windows.allSatisfy { $0.end - $0.start <= VideoSceneSelection.maxWindowSeconds + 1.01 && $0.times.count <= VideoSceneSelection.maxWindowFrames })
        #expect(windows.allSatisfy { zip($0.times, $0.times.dropFirst()).allSatisfy { $0 < $1 } })
    }

    @Test func poolTimesStayInsideTheVideo() {
        let times = VideoSceneSelection.poolTimes(duration: 2.2)
        #expect(times == [0, 0.5, 1.0, 1.5, 2.0])
        #expect(VideoSceneSelection.poolTimes(duration: 0).isEmpty)
    }
}

struct MergedCaptionTagParsingTests {
    @Test func readsDescriptionFilenameAndTagsFromOneReply() {
        let parsed = DeepAnalyze.parse(rawOutput: "DESCRIPTION: A boy swings a bat at a youth baseball game.\nFILENAME: boy-swinging-bat\nTAGS: baseball, batter")
        #expect(parsed.description == "A boy swings a bat at a youth baseball game.")
        #expect(parsed.proposedName == "boy-swinging-bat")
        #expect(parsed.tags == ["baseball", "batter"])
        #expect(parsed.hasTagSection)
    }

    @Test func aReplyWithoutTagsLeavesTheSecondPassAvailable() {
        let parsed = DeepAnalyze.parse(rawOutput: "DESCRIPTION: A lake.\nFILENAME: mountain-lake")
        #expect(!parsed.hasTagSection)
        #expect(parsed.tags.isEmpty)
        #expect(parsed.proposedName == "mountain-lake")
    }

    @Test func systemPromptAsksForTagsOnlyWhenMerged() {
        let merged = DeepAnalyze.analysisSystemPrompt(mediaKind: .image, fileExtension: "jpg", hasRaster: true, faceNames: [], includeTags: true)
        let plain = DeepAnalyze.analysisSystemPrompt(mediaKind: .image, fileExtension: "jpg", hasRaster: true, faceNames: [])
        #expect(merged.contains("EXACTLY three sections") && merged.contains("TAGS:"))
        #expect(plain.contains("EXACTLY two sections") && !plain.contains("TAGS:"))
    }
}
