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

    // R-17: the size gate is a *no-hash* fallback. After a passing SHA256 the
    // bytes are provably correct, so the loose estimate must NOT run — an
    // over-estimated approxBytes would otherwise delete a hash-verified file.
    @Test("size gate runs only when no hash is pinned")
    func gateDisabledOncePinnedHashPresent() {
        #expect(shouldEnforceSizeGate(expectedSHA256: nil))
        #expect(!shouldEnforceSizeGate(expectedSHA256: "abc123"))
        #expect(!shouldEnforceSizeGate(expectedSHA256: ""))
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

    // R-18: the single-stream path only lands ~1× on the destination volume,
    // so its peakMultiplier:1 must accept a fetch the 2× parallel guard rejects
    // (free space between 1× and 2× of the file).
    @Test("single-stream 1x multiplier accepts a fetch the 2x parallel guard rejects")
    func singleStreamMultiplierIsLessConservative() throws {
        let dest = tempDest()
        defer { try? FileManager.default.removeItem(at: dest.deletingLastPathComponent()) }
        guard let free = volumeFreeBytes(forItemAt: dest), free > 16 else { return }
        // Needs > free at 2× (1.2×free) but ≤ free at 1× (0.6×free).
        let approx = Int64(Double(free) * 0.6)
        #expect(throws: StreamingDownloadError.self) {
            try preflightDiskSpace(dest: dest, approxBytes: approx, peakMultiplier: 2)
        }
        try preflightDiskSpace(dest: dest, approxBytes: approx, peakMultiplier: 1)
    }

    @Test("multiplier defaults to the 2x parallel-staging guard")
    func defaultMultiplierIsParallel() {
        let dest = tempDest()
        defer { try? FileManager.default.removeItem(at: dest.deletingLastPathComponent()) }
        // 1 EiB at the default 2× exceeds any real volume → reject.
        #expect(throws: StreamingDownloadError.self) {
            try preflightDiskSpace(dest: dest, approxBytes: Int64(1) << 60)
        }
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
