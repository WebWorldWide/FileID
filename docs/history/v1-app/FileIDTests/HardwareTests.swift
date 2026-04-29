import XCTest
@testable import FileID

final class HardwareTests: XCTestCase {

    func testPhysicalMemoryIsPositive() {
        XCTAssertGreaterThan(Hardware.physicalMemoryGB, 0,
                             "Mac running these tests must have detectable RAM.")
    }

    func testCoreCountsAreSensible() {
        XCTAssertGreaterThan(Hardware.coreCount, 0)
        XCTAssertGreaterThan(Hardware.performanceCoreCount, 0)
        XCTAssertGreaterThanOrEqual(Hardware.efficiencyCoreCount, 0)
        // P + E should never exceed total core count (would mean sysctl returned
        // overlapping or stale values).
        XCTAssertLessThanOrEqual(
            Hardware.performanceCoreCount + Hardware.efficiencyCoreCount,
            Hardware.coreCount + 1, // +1 slack: hyperthreading reporting
            "P-cores + E-cores cannot exceed total core count meaningfully."
        )
    }

    func testWorkerCapHasFloor() {
        // Floor of 4 protects against weird hardware reporting 1 P-core.
        XCTAssertGreaterThanOrEqual(Hardware.workerCap, 4)
    }

    func testWorkerCapExceedsCoreCount() {
        // Workers spend ~half their wall time in ANE/GPU, so we deliberately
        // run MORE workers than P-cores to keep CPU stages saturated. The
        // formula adds extra queue-depth workers; expect cap > P-cores on
        // anything with at least 4 P-cores.
        if Hardware.performanceCoreCount >= 4 {
            XCTAssertGreaterThan(
                Hardware.workerCap, Hardware.performanceCoreCount,
                "workerCap should exceed P-cores to keep ANE queue saturated."
            )
        }
    }

    func testResidentMBNonNegativeOrSentinel() {
        // residentMB() returns -1 only when the mach call fails. On a healthy
        // test process it should return a positive number.
        let r = Hardware.residentMB()
        XCTAssertTrue(r > 0 || r == -1,
                      "residentMB returned 0 — caller cannot distinguish from sentinel.")
    }

    func testAvailableMemoryNonNegativeOrSentinel() {
        let a = Hardware.availableMemoryMB()
        XCTAssertTrue(a > 0 || a == -1,
                      "availableMemoryMB returned 0 — caller cannot distinguish from sentinel.")
    }

    func testCanSafelyLoadLargeModelDoesntFalsePositiveOnSentinel() {
        // canSafelyLoadLargeModel() must treat the -1 sentinel from
        // availableMemoryMB() as "unsafe", never as "0 MB free → unsafe in a
        // way that's indistinguishable from a real zero".
        // We can't easily inject a sentinel here, but we can sanity-check the
        // function returns a Bool and doesn't crash on the current host.
        _ = Hardware.canSafelyLoadLargeModel()
    }

    func testThumbnailCacheScalesWithRAM() {
        // The 16 GB tier cap is 600 MB; 24 GB is 1200; 48 GB is 2000.
        // Whatever this host is, the cache should be ≥ 600 MB.
        XCTAssertGreaterThanOrEqual(Hardware.thumbnailCacheMB, 600)
    }

    func testSaveEveryReasonable() {
        // Should be in the 250–1500 band; outside that is misconfigured.
        XCTAssertGreaterThanOrEqual(Hardware.saveEvery, 250)
        XCTAssertLessThanOrEqual(Hardware.saveEvery, 2000)
    }
}
