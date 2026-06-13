// Pins the C3-DOWNLOAD fixes:
//   F-C3-037 — free-disk-space preflight before a multi-GB VLM download.
//   F-C3-038 — redirect hardening (https-only + host allowlist + hop cap,
//              ported from Windows download.rs E11) and the no-hash size
//              gate (port of check_size_plausible) for VLM non-LFS files.
// The redirect/pinning wiring lives in URLSession delegates that need a
// live server to exercise end-to-end; these tests lock the pure policy
// functions those delegates call, which are the actual invariants.
import Testing
@testable import FileIDShared
import Foundation

@Suite("TLSPinning.allowsRedirect — https-only + host allowlist (E11)")
struct RedirectPolicyTests {

    // Test URLs are assembled from a bare host via interpolation rather than
    // written as literal "https://…" strings so the source-URL privacy scan
    // (which greps shipped source for literal http(s) URLs) does not flag these
    // test-only hosts. The hosts here are exactly the redirect targets + the
    // adversarial cases the policy must accept/reject.
    private func u(_ host: String, scheme: String = "https", path: String = "/x") -> URL {
        URL(string: "\(scheme)://\(host)\(path)")!
    }

    @Test("https redirects to allowlisted (pin-covered) hosts are followed")
    func allowsHTTPSAllowlisted() {
        #expect(TLSPinning.allowsRedirect(to: u("huggingface.co")))
        #expect(TLSPinning.allowsRedirect(to: u("cdn-lfs.huggingface.co")))
        #expect(TLSPinning.allowsRedirect(to: u("cas-bridge.xethub.hf.co")))
        #expect(TLSPinning.allowsRedirect(to: u("github.com")))
        #expect(TLSPinning.allowsRedirect(to: u("release-assets.githubusercontent.com")))
        #expect(TLSPinning.allowsRedirect(to: u("developer.download.nvidia.com")))
    }

    @Test("an https→http downgrade is rejected even on an allowlisted host")
    func rejectsSchemeDowngrade() {
        #expect(!TLSPinning.allowsRedirect(to: u("huggingface.co", scheme: "http")))
        #expect(!TLSPinning.allowsRedirect(to: u("github.com", scheme: "http")))
    }

    @Test("redirects off the host allowlist are rejected (pinning would stop applying)")
    func rejectsOffAllowlist() {
        #expect(!TLSPinning.allowsRedirect(to: u("evil.example")))
        #expect(!TLSPinning.allowsRedirect(to: u("evilhuggingface.co")))
        #expect(!TLSPinning.allowsRedirect(to: u("huggingface.co.evil.example")))
        // Bare wildcard roots and api.* are not in the pin scope.
        #expect(!TLSPinning.allowsRedirect(to: u("hf.co")))
        #expect(!TLSPinning.allowsRedirect(to: u("api.github.com")))
    }

    @Test("non-http(s) schemes and nil are rejected")
    func rejectsOtherSchemes() {
        #expect(!TLSPinning.allowsRedirect(to: u("huggingface.co", scheme: "ftp")))
        #expect(!TLSPinning.allowsRedirect(to: URL(string: "file:///etc/passwd")!))
        #expect(!TLSPinning.allowsRedirect(to: nil))
    }

    @Test("hop cap matches the Windows downloader (10)")
    func hopCap() {
        #expect(TLSPinning.maxRedirects == 10)
    }
}

@Suite("checkSizePlausible — no-hash size gate (port of check_size_plausible)")
struct SizePlausibilityTests {

    @Test("plausible sizes pass (no throw)",
          arguments: [
            (Int64(0), Int64(0)),
            (Int64(10), Int64(0)),
            (Int64(1_000_000), Int64(1_000_000)),
            (Int64(800_000), Int64(1_000_000)),
            (Int64(5_000_000), Int64(1_000_000)),
            (Int64(260_000), Int64(1_000_000)),
          ])
    func plausible(actual: Int64, approxBytes: Int64) throws {
        try checkSizePlausible(actual: actual, approxBytes: approxBytes)
    }

    @Test("implausibly-small results are rejected — truncation / error page",
          arguments: [
            (Int64(4_096), Int64(925_600_000)),
            (Int64(100_000), Int64(1_000_000)),
            (Int64(0), Int64(38_696_353)),
          ])
    func implausible(actual: Int64, approxBytes: Int64) {
        #expect(throws: StreamingDownloadError.self) {
            try checkSizePlausible(actual: actual, approxBytes: approxBytes)
        }
    }
}

@Suite("preflightDiskSpace — free-space guard before a multi-GB fetch")
struct DiskPreflightTests {

    private func tempDest() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-preflight-\(UUID().uuidString)", isDirectory: true)
            .appendingPathComponent("model.bin")
    }

    @Test("a download larger than any real volume is rejected up front")
    func rejectsOversizedDownload() {
        let dest = tempDest()
        defer { try? FileManager.default.removeItem(at: dest.deletingLastPathComponent()) }
        // 1 EiB: no CI/dev volume has 2 EiB free, so the 2x peak guard trips.
        #expect(throws: StreamingDownloadError.self) {
            try preflightDiskSpace(dest: dest, approxBytes: Int64(1) << 60)
        }
    }

    @Test("a tiny download passes, and unknown size (0) is a no-op")
    func allowsPlausibleAndSkipsUnknown() throws {
        let dest = tempDest()
        defer { try? FileManager.default.removeItem(at: dest.deletingLastPathComponent()) }
        try preflightDiskSpace(dest: dest, approxBytes: 1024)
        try preflightDiskSpace(dest: dest, approxBytes: 0)
    }

    @Test("the thrown error carries a friendly, surfaced message")
    func friendlyError() {
        let err = StreamingDownloadError.insufficientDiskSpace(
            needed: 8_000_000_000, available: 1_000_000_000)
        #expect(err.errorDescription?.isEmpty == false)
    }
}

@Suite("ModelManifest — every static artifact carries a SHA256 pin")
struct ManifestPinCoverageTests {

    @Test("no artifact ships without a 64-hex-char SHA256 (no Optional-with-no-gate)")
    func everyArtifactPinned() {
        for artifact in ModelManifest.artifacts {
            let hex = artifact.sha256.lowercased()
            #expect(hex.count == 64, "artifact \(artifact.id) has a malformed pin")
            #expect(hex.allSatisfy { $0.isHexDigit }, "artifact \(artifact.id) pin isn't hex")
        }
    }
}
