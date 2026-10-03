import Foundation
import CryptoKit

public enum CLIPEmbeddingSpace {
    public static let dimension = 512
    public static let modelID: String = {
        let ids = ["clip_vitb32_image", "clip_vitb32_text", "clip_bpe_vocab", "clip_bpe_merges"]
        let artifacts = ids.map { id in
            id + ":" + (ModelManifest.artifacts.first { $0.id == id }?.sha256 ?? "missing")
        }
        let descriptor = (["clip-rgb-stretch224-bpe77-l2-v1"] + artifacts).joined(separator: "|")
        let digest = SHA256.hash(data: Data(descriptor.utf8)).map { String(format: "%02x", $0) }.joined()
        return "clip-vitb32-openai-v1:" + digest
    }()

    public static func verifyArtifact(at url: URL, id: String) -> Bool {
        guard let artifact = ModelManifest.artifacts.first(where: { $0.id == id }) else { return false }
        return ModelArtifactVerification.verifyFile(at: url, sha256: artifact.sha256)
    }

    public static func vector(from data: Data) -> [Float]? {
        guard data.count == dimension * 4 else { return nil }
        let values = data.withUnsafeBytes { bytes in
            (0..<dimension).map { index in
                Float(bitPattern: UInt32(littleEndian: bytes.loadUnaligned(fromByteOffset: index * 4, as: UInt32.self)))
            }
        }
        guard values.allSatisfy(\.isFinite) else { return nil }
        let squaredNorm = values.reduce(0.0) { $0 + Double($1) * Double($1) }
        guard abs(squaredNorm - 1) <= 0.02 else { return nil }
        return values
    }
}

public enum ModelArtifactVerification {
    public static func verifyFile(at url: URL, sha256: String) -> Bool {
        guard url.isFileURL, sha256.count == 64, sha256.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              let properties = try? url.resolvingSymlinksInPath().resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey]),
              properties.isRegularFile == true, (properties.fileSize ?? 0) > 0 else { return false }
        return (try? sha256HexOfFile(at: url)) == sha256
    }
}
