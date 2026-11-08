import Foundation
import Testing
@testable import FileIDEngine

@Suite("Compiled CoreML model cache")
struct ORTSessionsTests {
    private let digest = String(repeating: "ab", count: 32)

    @Test("each model file, format and runtime gets its own folder in engine state")
    func folders() {
        let program = ORTSessions.CoreMLCache(modelDigest: digest, mlProgram: true, subgraphs: true)
        let network = ORTSessions.CoreMLCache(modelDigest: digest, mlProgram: false, subgraphs: false)
        let other = ORTSessions.CoreMLCache(modelDigest: String(repeating: "cd", count: 32), mlProgram: true, subgraphs: true)
        let root = EngineStateRoot.directory.appendingPathComponent("ModelCache/CoreML", isDirectory: true).standardizedFileURL.path
        for cache in [program, network, other] {
            #expect(cache.directory.deletingLastPathComponent().standardizedFileURL.path == root)
            #expect(cache.providerOptions["ModelCacheDirectory"] == cache.directory.path)
        }
        #expect(Set([program.directory, network.directory, other.directory]).count == 3)
        #expect(program.directory.lastPathComponent.hasPrefix(String(digest.prefix(32))))
        #expect(program.providerOptions["ModelFormat"] == "MLProgram")
        #expect(network.providerOptions["ModelFormat"] == "NeuralNetwork")
        #expect(network.providerOptions["MLComputeUnits"] == nil)
        var gpu = network
        gpu.cpuAndGPU = true
        #expect(gpu.providerOptions["MLComputeUnits"] == "CPUAndGPU")
        #expect(gpu.directory != network.directory)
    }
}
