// Vectors computed by running the Windows engine's stable_path_hash
// (Rust DefaultHasher = SipHash-1-3, zero keys) — mirrored by the
// stable_path_hash_pinned_vectors test in path_safety.rs so neither
// side can drift without a test failing.
import Testing
@testable import FileIDShared

@Suite("StablePathHash")
struct StablePathHashTests {

    @Test("matches Rust stable_path_hash pinned vectors")
    func pinnedVectors() {
        #expect(StablePathHash.hash("") == 3_476_900_567_878_811_119)
        #expect(StablePathHash.hash("a") == 8_186_225_505_942_432_243)
        #expect(StablePathHash.hash("/Users/adam/Photos/IMG_0001.JPG")
                == -6_847_549_264_798_039_763)
        #expect(StablePathHash.hash("C:\\Users\\Adam\\Pictures\\Photo.JPG")
                == -5_418_614_373_936_508_534)
        #expect(StablePathHash.hash("/Users/ådam/Désktop/Café.jpg")
                == 6_025_210_603_525_090_388)
        #expect(StablePathHash.hash("/Users/adam/Photos/家族写真.jpg")
                == -1_257_796_233_084_950_905)
        #expect(StablePathHash.hash(
            "/Users/adam/Library/Mobile Documents/com~apple~CloudDocs/Tax 2024 (final).pdf")
                == 1_387_562_067_336_403_736)
    }

    @Test("ASCII case-insensitive, multi-byte scalars untouched")
    func caseInsensitivity() {
        #expect(StablePathHash.hash("/users/adam/photos/img_0001.jpg")
                == StablePathHash.hash("/Users/Adam/Photos/IMG_0001.JPG"))
        #expect(StablePathHash.hash("/Users/ådam/Désktop/Café.jpg")
                == StablePathHash.hash("/users/ådam/désktop/café.jpg"))
        #expect(StablePathHash.hash("/a/Å.jpg") != StablePathHash.hash("/a/å.jpg"))
    }

    @Test("deterministic across calls")
    func deterministic() {
        let p = "/Volumes/NAS/Photos/2024/IMG_4521.HEIC"
        #expect(StablePathHash.hash(p) == StablePathHash.hash(p))
    }
}
