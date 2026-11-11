import Foundation
import MLX
import Testing
@testable import FileIDEngine
@testable import FileIDVLMCore

/// MLX needs its Metal library beside the binary that links it; without one
/// these tests skip (CI builds have none).
private let metallibBesideTests: Bool = {
    var info = Dl_info()
    guard dladdr(#dsohandle, &info) != 0, let name = info.dli_fname else { return false }
    let folder = URL(fileURLWithPath: String(cString: name)).deletingLastPathComponent()
    return ["mlx.metallib", "default.metallib"].contains {
        FileManager.default.fileExists(atPath: folder.appendingPathComponent($0).path)
    }
}()

@Suite("MLX errors fail the work, not the engine", .enabled(if: metallibBesideTests))
struct MLXErrorScopeTests {
    @Test func aBroadcastErrorIsThrown() async {
        await #expect(throws: MLXError.self) {
            try await catchingMLX {
                _ = MLXArray(0 ..< 10, [2, 5]) + MLXArray(0 ..< 15, [3, 5])
            }
        }
    }

    @Test func anAnalysisWithAnMLXErrorFailsThatFile() async {
        let result = await DeepAnalyze.shared.guardedAnalysis {
            _ = MLXArray(0 ..< 10, [2, 5]) + MLXArray(0 ..< 15, [3, 5])
            return DeepAnalyze.AnalysisResult.make(description: "A caption.", proposedName: nil)
        }
        #expect(result.description.hasPrefix("Inference failed"))
        #expect(DeepAnalyzeRunner.isAnalysisFailure(result.description))
    }
}
