// Locks the compiled TLSPinning table to the canonical
// shared/security/tls-pins.json, and verifies the SPKI hashing helper
// (ASN.1 header reconstruction + SHA256) against fixture certificates
// whose SPKI hashes were precomputed with openssl at authoring time:
//   openssl x509 -in <pem> -pubkey -noout \
//     | openssl pkey -pubin -outform der \
//     | openssl dgst -sha256 -binary | base64
import Testing
@testable import FileIDShared
import Foundation
import Security

private func repoRootURL() -> URL {
    URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()   // SharedTests
        .deletingLastPathComponent()   // Tests
        .deletingLastPathComponent()   // apple
        .deletingLastPathComponent()   // platforms
        .deletingLastPathComponent()   // repo root
}

private func certificate(at pemURL: URL) throws -> SecCertificate {
    let pem = try String(contentsOf: pemURL, encoding: .utf8)
    let base64 = pem
        .components(separatedBy: .newlines)
        .filter { !$0.hasPrefix("-----") && !$0.isEmpty }
        .joined()
    let der = try #require(Data(base64Encoded: base64), "fixture PEM didn't decode: \(pemURL.lastPathComponent)")
    return try #require(SecCertificateCreateWithData(nil, der as CFData),
                        "fixture isn't a valid DER certificate: \(pemURL.lastPathComponent)")
}

@Suite("TLSPinning — locked to shared/security/tls-pins.json")
struct TLSPinsJSONLockTests {

    private func pinsJSON() throws -> [String: Any] {
        let url = repoRootURL().appendingPathComponent("shared/security/tls-pins.json")
        let data = try Data(contentsOf: url)
        let object = try JSONSerialization.jsonObject(with: data)
        return try #require(object as? [String: Any])
    }

    @Test("appliesToHosts matches the JSON exactly")
    func hostsMatchJSON() throws {
        let hosts = try #require(pinsJSON()["appliesToHosts"] as? [String])
        #expect(TLSPinning.appliesToHosts == hosts)
    }

    @Test("pinned roots match the JSON (both directions)")
    func rootsMatchJSON() throws {
        let rows = try #require(pinsJSON()["roots"] as? [[String: Any]])
        var expected: [String: String] = [:]
        for row in rows {
            let slug = try #require(row["slug"] as? String)
            let spki = try #require(row["spkiSHA256Base64"] as? String)
            expected[slug] = spki
        }
        #expect(TLSPinning.pinnedRoots.count == expected.count)
        for pin in TLSPinning.pinnedRoots {
            #expect(expected[pin.slug] == pin.spkiSHA256Base64,
                    "SPKI drifted for \(pin.slug)")
        }
        let tableSlugs = Set(TLSPinning.pinnedRoots.map(\.slug))
        for slug in expected.keys {
            #expect(tableSlugs.contains(slug), "JSON root missing from table: \(slug)")
        }
    }
}

@Suite("TLSPinning — SPKI hashing")
struct SPKIHashingTests {

    @Test("fixture cert SPKI hashes match openssl-precomputed values",
          arguments: [
            ("spki-test-ec-p256.pem",  "jEQCIJHijQ3/7mm4rdPGpMgDVOhc0NF8PZ3O5oCS5sQ="),
            ("spki-test-ec-p384.pem",  "EAtzfrMFleiSpZ0SmtZlwuNPhOcVWWPABpkSCqADMr0="),
            ("spki-test-rsa-2048.pem", "Pl4Q6IRp5LF2FNt1fiiVaIchUKCQJ5Nx8UWJaYf2Njw="),
            ("spki-test-rsa-4096.pem", "LZd1l1sQs/Wuez19K6w18XHytgVxd1ELoEd4a+xbsdM="),
          ])
    func fixtureSPKIHash(fixture: String, expected: String) throws {
        let pemURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("Fixtures")
            .appendingPathComponent(fixture)
        let cert = try certificate(at: pemURL)
        let actual = try #require(TLSPinning.spkiSHA256Base64(of: cert),
                                  "SPKI helper returned nil for \(fixture)")
        #expect(actual == expected)
    }

    @Test("every pinned-root PEM hashes to its tls-pins.json SPKI via the Swift helper")
    func pinnedRootPEMsHash() throws {
        let rootsDir = repoRootURL().appendingPathComponent("shared/security/pinned-roots")
        let pems = try FileManager.default
            .contentsOfDirectory(at: rootsDir, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "pem" }
        #expect(pems.count == TLSPinning.pinnedRoots.count)
        let bySlug = Dictionary(uniqueKeysWithValues:
            TLSPinning.pinnedRoots.map { ($0.slug, $0.spkiSHA256Base64) })
        for pemURL in pems {
            let slug = pemURL.deletingPathExtension().lastPathComponent
            let expected = try #require(bySlug[slug], "no compiled pin for \(slug)")
            let cert = try certificate(at: pemURL)
            let actual = try #require(TLSPinning.spkiSHA256Base64(of: cert),
                                      "SPKI helper returned nil for \(slug)")
            #expect(actual == expected, "SPKI mismatch for \(slug)")
        }
    }
}

@Suite("TLSPinning — host matching")
struct TLSPinningHostMatchTests {

    @Test("exact hosts, wildcard labels, and non-matches")
    func hostMatching() {
        #expect(TLSPinning.hostMatches("huggingface.co"))
        #expect(TLSPinning.hostMatches("HuggingFace.CO"))
        #expect(TLSPinning.hostMatches("cdn-lfs.huggingface.co"))
        #expect(TLSPinning.hostMatches("cas-bridge.xethub.hf.co"))
        #expect(TLSPinning.hostMatches("github.com"))
        #expect(TLSPinning.hostMatches("release-assets.githubusercontent.com"))
        #expect(TLSPinning.hostMatches("developer.download.nvidia.com"))

        // Wildcards require at least one extra label.
        #expect(!TLSPinning.hostMatches("hf.co"))
        #expect(!TLSPinning.hostMatches("githubusercontent.com"))
        // Suffix tricks must not match.
        #expect(!TLSPinning.hostMatches("evilhuggingface.co"))
        #expect(!TLSPinning.hostMatches("huggingface.co.evil.example"))
        // Unpinned hosts get default handling.
        #expect(!TLSPinning.hostMatches("api.github.com"))
        #expect(!TLSPinning.hostMatches("example.com"))
    }
}
