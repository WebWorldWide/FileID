import Testing
@testable import FileIDShared

@Suite("Local archive entry safety")
struct LocalArchiveSafetyTests {
    @Test("safe relative entries are accepted")
    func safeNames() {
        #expect(LocalArchiveSafety.entryNameIsSafe("mobileclip_image/model.mlmodelc/weights.bin"))
        #expect(LocalArchiveSafety.entryNameIsSafe("clip_text/vocab.json"))
    }

    @Test("absolute, traversal, Windows, NUL, and oversized entries are rejected")
    func unsafeNames() {
        for name in [
            "/tmp/escape",
            "../escape",
            "safe/../../escape",
            "\\\\server\\share\\escape",
            "C:/escape",
            "safe\\..\\escape",
            "safe\0escape",
            String(repeating: "a", count: 4_097),
        ] {
            #expect(!LocalArchiveSafety.entryNameIsSafe(name), "accepted unsafe name: \(name)")
        }
    }

    @Test("ordinary unencrypted status is accepted without substring confusion")
    func encryptionStatus() {
        #expect(LocalArchiveSafety.fileSecurityStatusIsUnencrypted("not encrypted"))
        #expect(LocalArchiveSafety.fileSecurityStatusIsUnencrypted("  NOT ENCRYPTED "))
        #expect(!LocalArchiveSafety.fileSecurityStatusIsUnencrypted("encrypted"))
        #expect(!LocalArchiveSafety.fileSecurityStatusIsUnencrypted("unknown"))
    }

    @Test("only regular files and directories pass Unix type preflight")
    func unixTypes() {
        #expect(LocalArchiveSafety.unixEntryTypeIsSafe("-rw-r--r--"))
        #expect(LocalArchiveSafety.unixEntryTypeIsSafe("drwxr-xr-x"))
        #expect(!LocalArchiveSafety.unixEntryTypeIsSafe("lrwxr-xr-x"))
        #expect(!LocalArchiveSafety.unixEntryTypeIsSafe("prw-r--r--"))
        #expect(!LocalArchiveSafety.unixEntryTypeIsSafe(""))
    }
}
