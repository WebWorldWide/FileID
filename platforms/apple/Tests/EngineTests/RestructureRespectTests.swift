import Testing
import Foundation
@testable import FileIDEngine

@Suite("Restructure keeps the user's organization")
struct RestructureRespectTests {
    private let root = "/Volumes/Adlon/Camera"
    private func move(_ id: Int64, _ from: String, _ to: String, bucket: String = "x") -> RestructureProposal {
        RestructureProposal(fileID: id, oldPath: root + "/" + from, newPath: root + "/" + to, bucket: bucket, confidence: "auto")
    }
    private func info(_ id: Int64, _ path: String, kind: String = "image", t: Double? = nil) -> RestructureRespect.FileInfo {
        .init(fileID: id, path: root + "/" + path, kind: kind, captureUnix: t)
    }

    @Test func genericAndCameraFolderNamesAreRecognised() {
        for name in ["Downloads", "DCIM", "100MSDCF", "New Folder 2", "untitled folder", "Misc"] {
            #expect(RestructureRespect.isGenericFolderName(name), "\(name)")
        }
        for name in ["Kyle Baseball Game 9-19-26", "Hunter", "Photos", "RAWs", "Taxes 2025"] {
            #expect(!RestructureRespect.isGenericFolderName(name), "\(name)")
        }
    }

    @Test func formatRolesFromFolderNamesAndFiles() {
        #expect(RestructureRespect.formatRole(ofFolderName: "RAWs") == .raw)
        #expect(RestructureRespect.formatRole(ofFolderName: "RAW") == .raw)
        #expect(RestructureRespect.formatRole(ofFolderName: "Edited JPEGs") == .jpeg)
        #expect(RestructureRespect.formatRole(ofFolderName: "Videos") == .video)
        #expect(RestructureRespect.formatRole(ofFolderName: "Photos") == .photo)
        #expect(RestructureRespect.formatRole(ofFolderName: "Hunter") == nil)
        #expect(RestructureRespect.formatRole(ofFile: "/a/DSC01.ARW", kind: "image") == .raw)
        #expect(RestructureRespect.formatRole(ofFile: "/a/DSC01.JPG", kind: "image") == .jpeg)
        #expect(RestructureRespect.formatRole(ofFile: "/a/C0031.MP4", kind: "video") == .video)
    }

    @Test func organizedFilesStayAndEventsNeverMix() {
        let files = [1: info(1, "Kyle Baseball Game 9-19-26/Photos/RAWs/DSC1.ARW"),
                     2: info(2, "Eckerts 10-4-26/Photos/JPEGs/DSC2.JPG")]
        let proposals = [
            move(1, "Kyle Baseball Game 9-19-26/Photos/RAWs/DSC1.ARW", "Kyle Baseball Game 9-20-26/Hunter/RAW/DSC1.ARW"),
            move(2, "Eckerts 10-4-26/Photos/JPEGs/DSC2.JPG", "SIUE Homecoming 9-26-26/Photos/Edited JPEGs/DSC2.JPG"),
        ]
        let result = RestructureRespect.apply(proposals, files: files.reduce(into: [:]) { $0[Int64($1.key)] = $1.value }, libraryRoot: root)
        #expect(result.kept.isEmpty)
        #expect(result.dropped == 2)
    }

    @Test func looseFilesAreOrganizedButNotIntoWrongFormatsOrJunkNames() {
        let files: [Int64: RestructureRespect.FileInfo] = [
            1: info(1, "IMG_1.JPG"), 2: info(2, "Downloads/IMG_2.JPG"), 3: info(3, "IMG_3.JPG"), 4: info(4, "Downloads/C1.MP4", kind: "video"),
        ]
        let proposals = [
            move(1, "IMG_1.JPG", "Photos/2026/September/IMG_1.JPG"),
            move(2, "Downloads/IMG_2.JPG", "Kyle Baseball Game 9-20-26/Hunter/RAW/IMG_2.JPG"),
            move(3, "IMG_3.JPG", "Dsc01548 Sunglasses/IMG_3.JPG"),
            move(4, "Downloads/C1.MP4", "Consumer_electronics Computer/C1.MP4"),
        ]
        let kept = RestructureRespect.apply(proposals, files: files, libraryRoot: root).kept.map(\.fileID)
        #expect(kept == [1])
    }

    @Test func misfiledFormatsMoveToTheirSiblingFolder() {
        let files = [
            info(1, "Eckerts 10-4-26/Photos/JPEGs/DSC9.ARW"),
            info(2, "Eckerts 10-4-26/Photos/JPEGs/DSC9.JPG"),
            info(3, "Eckerts 10-4-26/Photos/RAWs/DSC8.ARW"),
        ]
        let fixes = RestructureRespect.formatCorrections(files: files, root: root)
        #expect(fixes.count == 1)
        #expect(fixes.first?.newPath == root + "/Eckerts 10-4-26/Photos/RAWs/DSC9.ARW")
        #expect(RestructureRespect.keep(fixes[0], file: files[0], root: root))
    }

    @Test func looseFilesJoinTheEventTheyWereCapturedDuring() {
        let day: Double = 1_790_000_000
        var files = (0..<6).map { info(Int64($0), "Kyle Baseball Game 9-19-26/Photos/RAWs/K\($0).ARW", t: day + Double($0) * 600) }
        files += (0..<6).map { info(Int64(10 + $0), "Kyle Baseball Game 9-19-26/Photos/JPEGs/K\($0).JPG", t: day + Double($0) * 600) }
        files += (0..<6).map { info(Int64(20 + $0), "Eckerts 10-4-26/Photos/JPEGs/E\($0).JPG", t: day + 15 * 86_400 + Double($0) * 600) }
        let spans = RestructureRespect.eventSpans(files: files, root: root)
        #expect(spans.count == 2)
        let loose = info(99, "Downloads/DSC5.ARW", t: day + 1_200)
        #expect(RestructureRespect.eventDestination(for: loose, spans: spans) == root + "/Kyle Baseball Game 9-19-26/Photos/RAWs/DSC5.ARW")
        let unrelated = info(98, "Downloads/old.JPG", t: day - 200 * 86_400)
        #expect(RestructureRespect.eventDestination(for: unrelated, spans: spans) == nil)
    }

    @Test func cameraFilenamesAndTagIdentifiersAreNotFolderNames() {
        #expect(!RestructureRespect.isUsableFolderName("Dsc01548 Sunglasses"))
        #expect(!RestructureRespect.isUsableFolderName("Consumer_electronics Computer"))
        #expect(!RestructureRespect.isUsableFolderName("IMG 4821"))
        #expect(RestructureRespect.isUsableFolderName("Dogs"))
        #expect(RestructureRespect.isUsableFolderName("Lake Trip"))
    }
}
