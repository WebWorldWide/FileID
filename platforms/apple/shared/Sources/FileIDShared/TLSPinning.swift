// CA-allowlist TLS pinning for model-download egress. Compiled mirror
// of shared/security/tls-pins.json (locked to the JSON by
// SharedTests/TLSPinningTests.swift). Pin level is the ROOT CA's SPKI
// SHA256 — leaves rotate ~90 days and HF/GitHub move between CDNs;
// roots are stable for decades. Defense-in-depth alongside the
// per-artifact SHA256 pins in ModelManifest: full system trust
// evaluation runs FIRST, then any certificate in the evaluated chain
// must carry a pinned SPKI. Hosts outside `appliesToHosts` get default
// handling — pinning never widens trust, only narrows it.
import Foundation
import CryptoKit
import Security

public enum TLSPinning {

    public struct PinnedRoot: Sendable, Equatable {
        public let slug: String
        public let spkiSHA256Base64: String
    }

    public static let appliesToHosts: [String] = [
        "huggingface.co",
        "*.huggingface.co",
        "*.hf.co",
        "github.com",
        "*.githubusercontent.com",
        "developer.download.nvidia.com",
    ]

    public static let pinnedRoots: [PinnedRoot] = [
        PinnedRoot(slug: "amazon-root-ca-1",      spkiSHA256Base64: "++MBgDH5WGvL9Bcn5Be30cRcL0f5O+NyoXuWtQdX1aI="),
        PinnedRoot(slug: "amazon-root-ca-2",      spkiSHA256Base64: "f0KW/FtqTjs108NpYj42SrGvOB2PpxIVM8nWxjPqJGE="),
        PinnedRoot(slug: "amazon-root-ca-3",      spkiSHA256Base64: "NqvDJlas/GRcYbcWE8S/IceH9cq77kg0jVhZeAPXq8k="),
        PinnedRoot(slug: "amazon-root-ca-4",      spkiSHA256Base64: "9+ze1cZgR9KO1kZrVDxA4HQ6voHRCSVNz4RdTCx4U8U="),
        PinnedRoot(slug: "starfield-services-g2", spkiSHA256Base64: "KwccWaCgrnaw6tsrrSO61FgLacNgG2MMLq8GE6+oP5I="),
        PinnedRoot(slug: "isrg-root-x1",          spkiSHA256Base64: "C5+lpZ7tcVwmwQIMcRtPbsQtWLABXhQzejna0wHFr8M="),
        PinnedRoot(slug: "isrg-root-x2",          spkiSHA256Base64: "diGVwiVYbubAI3RW4hB9xU8e/CH2GnkuvVFZE8zmgzI="),
        PinnedRoot(slug: "usertrust-ecc",         spkiSHA256Base64: "ICGRfpgmOUXIWcQ/HXPLQTkFPEFPoDyjvH7ohhQpjzs="),
        PinnedRoot(slug: "usertrust-rsa",         spkiSHA256Base64: "x4QzPSC810K5/cMjb05Qm4k3Bw5zBn4lTdO/nEW/Td4="),
        PinnedRoot(slug: "digicert-global-g2",    spkiSHA256Base64: "i7WTqTvh0OioIruIfFR4kMPnBqrS2rdiVPl/s2uC/CY="),
        PinnedRoot(slug: "digicert-global-g3",    spkiSHA256Base64: "uUwZgwDOxcBXrQcntwu+kYFpkiVkOaezL0WYEZ3anJc="),
    ]

    private static let pinnedSPKIs: Set<String> = Set(pinnedRoots.map(\.spkiSHA256Base64))

    /// Escape hatch for TLS-intercepting corporate proxies: reverts the
    /// matching hosts to plain system-root validation. Changes validation
    /// only — never adds egress. Logged loudly exactly once.
    static let pinningDisabled: Bool = {
        guard ProcessInfo.processInfo.environment["FILEID_DISABLE_TLS_PINNING"] == "1" else {
            return false
        }
        FileHandle.standardError.write(Data(
            "WARNING: FILEID_DISABLE_TLS_PINNING=1 — TLS CA pinning for model downloads is DISABLED; falling back to system trust roots. Unset this variable unless you are debugging behind a TLS-intercepting proxy.\n".utf8))
        return true
    }()

    public static func evaluate(
        challenge: URLAuthenticationChallenge
    ) -> (URLSession.AuthChallengeDisposition, URLCredential?) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust else {
            return (.performDefaultHandling, nil)
        }
        guard hostMatches(challenge.protectionSpace.host) else {
            return (.performDefaultHandling, nil)
        }
        if pinningDisabled {
            return (.performDefaultHandling, nil)
        }
        var evalError: CFError?
        guard SecTrustEvaluateWithError(trust, &evalError) else {
            return (.cancelAuthenticationChallenge, nil)
        }
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate] else {
            return (.cancelAuthenticationChallenge, nil)
        }
        for certificate in chain {
            if let spki = spkiSHA256Base64(of: certificate), pinnedSPKIs.contains(spki) {
                return (.useCredential, URLCredential(trust: trust))
            }
        }
        return (.cancelAuthenticationChallenge, nil)
    }

    /// `*.` patterns require at least one extra label — `*.hf.co`
    /// matches `cas-bridge.xethub.hf.co` but not `hf.co` itself.
    static func hostMatches(_ host: String) -> Bool {
        let h = host.lowercased()
        for pattern in appliesToHosts {
            if pattern.hasPrefix("*.") {
                let suffix = String(pattern.dropFirst(1))
                if h.hasSuffix(suffix), h.count > suffix.count { return true }
            } else if h == pattern {
                return true
            }
        }
        return false
    }

    // MARK: - SPKI hashing

    static func spkiSHA256Base64(of certificate: SecCertificate) -> String? {
        guard let key = SecCertificateCopyKey(certificate) else { return nil }
        return spkiSHA256Base64(of: key)
    }

    /// SecKeyCopyExternalRepresentation returns the bare key material
    /// (PKCS#1 RSAPublicKey for RSA, the X9.63 uncompressed point for
    /// EC) — NOT the SubjectPublicKeyInfo that pins hash over. Prepend
    /// the fixed ASN.1 SPKI header for the key's type+size to rebuild
    /// the exact DER `openssl pkey -pubin -outform der` emits.
    static func spkiSHA256Base64(of key: SecKey) -> String? {
        guard let attrs = SecKeyCopyAttributes(key) as? [CFString: Any],
              let keyType = attrs[kSecAttrKeyType] as? String,
              let keySizeInBits = (attrs[kSecAttrKeySizeInBits] as? NSNumber)?.intValue,
              let header = spkiHeader(keyType: keyType, keySizeInBits: keySizeInBits),
              let keyData = SecKeyCopyExternalRepresentation(key, nil) as Data? else {
            return nil
        }
        var spki = Data(header)
        spki.append(keyData)
        return Data(SHA256.hash(data: spki)).base64EncodedString()
    }

    private static func spkiHeader(keyType: String, keySizeInBits: Int) -> [UInt8]? {
        let rsa = kSecAttrKeyTypeRSA as String
        let ec = kSecAttrKeyTypeECSECPrimeRandom as String
        switch (keyType, keySizeInBits) {
        case (rsa, 2048):
            return [0x30, 0x82, 0x01, 0x22, 0x30, 0x0d, 0x06, 0x09,
                    0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01,
                    0x05, 0x00, 0x03, 0x82, 0x01, 0x0f, 0x00]
        case (rsa, 4096):
            return [0x30, 0x82, 0x02, 0x22, 0x30, 0x0d, 0x06, 0x09,
                    0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01,
                    0x05, 0x00, 0x03, 0x82, 0x02, 0x0f, 0x00]
        case (ec, 256):
            return [0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48,
                    0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48,
                    0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00]
        case (ec, 384):
            return [0x30, 0x76, 0x30, 0x10, 0x06, 0x07, 0x2a, 0x86, 0x48,
                    0xce, 0x3d, 0x02, 0x01, 0x06, 0x05, 0x2b, 0x81, 0x04,
                    0x00, 0x22, 0x03, 0x62, 0x00]
        default:
            return nil
        }
    }
}

/// Challenge-only URLSession delegate for plain `session.data(for:)`
/// call sites (e.g. the HF tree listing) that don't need download
/// callbacks. `pinningRejected` lets the caller distinguish a pinning
/// cancellation (surfaces as URLError.cancelled) from a user cancel.
public final class TLSPinningSessionDelegate: NSObject, URLSessionDelegate, @unchecked Sendable {
    public private(set) var pinningRejected = false

    public func urlSession(_ session: URLSession,
                           didReceive challenge: URLAuthenticationChallenge,
                           completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        let (disposition, credential) = TLSPinning.evaluate(challenge: challenge)
        if disposition == .cancelAuthenticationChallenge { pinningRejected = true }
        completionHandler(disposition, credential)
    }
}
