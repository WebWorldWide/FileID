import Foundation
import Testing
@testable import FileIDShared

@Suite("File modification time as the catalog stores it")
struct FileModificationTimeTests {
    /// (seconds, nanoseconds, bits). The Rust engine's `catalog_time_tests` pins the same table.
    /// Most rows are timestamps whose double `Date.timeIntervalSince1970` rounds one bit differently.
    static let table: [(Int64, Int64, UInt64)] = [
        (1_724_989_629, 833_179_165, 0x41d9_b450_2f75_52cf),
        (1_603_520_313, 785_102_536, 0x41d7_e4f1_ce72_3f1f),
        (1_704_538_783, 171_373_719, 0x41d9_664c_a7ca_f7c9),
        (1_700_000_000, 0, 0x41d9_54fc_4000_0000),
        (1_719_409_671, 136_042_265, 0x41d9_9f07_01c8_b4eb),
        (1_635_498_135, 1_905_740, 0x41d8_5eee_25c0_1f39),
        (1_794_374_740, 262_792_743, 0x41da_bcff_1510_d199),
        (1_794_731_299, 518_318_480, 0x41da_be5b_48e1_2c21),
        (0, 0, 0),
        (0, 1, 0x3e11_2e0b_e826_d695),
        (1, 999_999_999, 0x3fff_ffff_ffbb_47d0),
        (2_147_483_648, 500_000_000, 0x41e0_0000_0010_0000),
        (4_102_444_800, 123_456_789, 0x41ee_90ca_e003_f35c)
    ]

    @Test("seconds plus nanoseconds over a billion gives the Rust engine's double")
    func arithmeticTable() throws {
        for (seconds, nanoseconds, bits) in Self.table {
            let value = try #require(FileModificationTime.unixSeconds(seconds: seconds, nanoseconds: nanoseconds))
            #expect(value.bitPattern == bits, "\(seconds).\(nanoseconds)")
        }
    }

    @Test("a time before 1970 or a malformed one has no value")
    func noValue() {
        #expect(FileModificationTime.unixSeconds(seconds: -1, nanoseconds: 500_000_000) == nil)
        #expect(FileModificationTime.unixSeconds(seconds: 5, nanoseconds: -1) == nil)
        #expect(FileModificationTime.unixSeconds(seconds: 5, nanoseconds: 1_000_000_000) == nil)
        #expect(FileModificationTime.unixSeconds(path: "/nonexistent-\(UUID().uuidString)") == nil)
    }

    @Test("a file's stat time reads back as the table's double, through a path and a symlink")
    func readsFromDisk() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-mtime-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("a.bin"), link = folder.appendingPathComponent("link")
        try Data([1]).write(to: file)
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: file)
        for (seconds, nanoseconds, bits) in Self.table where seconds > 0 {
            var times = [timespec(tv_sec: Int(seconds), tv_nsec: Int(nanoseconds)), timespec(tv_sec: Int(seconds), tv_nsec: Int(nanoseconds))]
            #expect(utimensat(AT_FDCWD, file.path, &times, 0) == 0)
            #expect(FileModificationTime.unixSeconds(path: file.path)?.bitPattern == bits)
            #expect(FileModificationTime.unixSeconds(path: link.path)?.bitPattern == bits)
            #expect(FileModificationTime.unixSeconds(path: link.path, followLinks: false) != nil)
        }
    }

    @Test("revisions match when the size is equal and the times are the same instant")
    func revisionMatch() {
        let plain: UInt64 = 4_745_314_456_662_256_673, rounded: UInt64 = 4_745_314_456_662_256_674
        let later = Double(bitPattern: plain) + 0.001
        #expect(SourceRevisionMatch.same("100:\(plain)", "100:\(rounded)"))
        #expect(SourceRevisionMatch.same("100:\(rounded)", "100:\(plain)"))
        #expect(SourceRevisionMatch.same("100:\(plain)", "100:\(plain)"))
        #expect(!SourceRevisionMatch.same("100:\(plain)", "101:\(rounded)"))
        #expect(!SourceRevisionMatch.same("100:\(plain)", "100:\(later.bitPattern)"))
        #expect(SourceRevisionMatch.same("100:unknown", "100:unknown"))
        #expect(!SourceRevisionMatch.same("100:unknown", "100:\(plain)"))
        #expect(!SourceRevisionMatch.same("100:\(plain)", "garbage"))
        #expect(!SourceRevisionMatch.same("100:\(Double.nan.bitPattern)", "100:\(Double.nan.bitPattern + 1)"))
    }

    @Test("the face source revision is built from the stat time, not a Date")
    func faceRevisionUsesStatTime() throws {
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-face-rev-\(UUID().uuidString).jpg")
        try Data([1, 2, 3]).write(to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        var times = [timespec(tv_sec: 1_724_989_629, tv_nsec: 833_179_165), timespec(tv_sec: 1_724_989_629, tv_nsec: 833_179_165)]
        #expect(utimensat(AT_FDCWD, file.path, &times, 0) == 0)
        #expect(FaceCacheProvenance.currentSourceRevision(path: file.path) == "3:\(0x41d9_b450_2f75_52cf as UInt64)")
    }
}
