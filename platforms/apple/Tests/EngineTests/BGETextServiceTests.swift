import Testing
import Foundation
@testable import FileIDEngine

// On-device verification of the BGE ONNX inference. Conditional on the model being
// installed (so CI without the ~135 MB download still passes); when present it confirms
// the embedder loads, returns a 384-d L2-normalized vector, and is semantically
// meaningful — two same-topic documents must be closer than two different-topic ones.
@Suite("BGE text embedder (on-device, model-gated)")
struct BGETextServiceTests {
    /// Only an explicit scratch copy (`FILEID_MODELS_DIR`), never the owner's live Models folder.
    private var modelDir: URL? {
        guard let root = ProcessInfo.processInfo.environment["FILEID_MODELS_DIR"], root.hasPrefix("/") else { return nil }
        return URL(fileURLWithPath: root, isDirectory: true).appendingPathComponent("bge_text")
    }

    private func cos(_ a: [Float], _ b: [Float]) -> Float {
        zip(a, b).reduce(0) { $0 + $1.0 * $1.1 }   // both are L2-normalized
    }

    @Test("loads + embeds: same-topic docs are closer than different-topic")
    func semanticOrdering() {
        guard let modelDir, FileManager.default.fileExists(atPath: modelDir.appendingPathComponent("bge_small.onnx").path) else {
            return // no scratch model copy in this environment — skip
        }
        let svc = BGETextService.shared
        #expect(svc.load(modelDir: modelDir))

        let physics1 = svc.embed("Physics homework solving for velocity and acceleration under constant force")
        let physics2 = svc.embed("Kinematics lab report measuring acceleration of a cart down an incline")
        let english = svc.embed("English essay analyzing the themes of ambition in Shakespeare's Macbeth")

        guard let p1 = physics1, let p2 = physics2, let en = english else {
            Issue.record("embed returned nil for an installed model")
            return
        }
        #expect(p1.count == 384)
        // L2-normalized → self-cosine ≈ 1.
        #expect(abs(cos(p1, p1) - 1.0) < 1e-3)
        // The two physics docs must be more similar than physics vs. english.
        #expect(cos(p1, p2) > cos(p1, en))
    }
}
