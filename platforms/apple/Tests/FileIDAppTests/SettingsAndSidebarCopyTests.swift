import Foundation
import Testing
import FileIDShared
@testable import FileID

@Suite("Settings, Locations, sidebar and People copy")
struct SettingsAndSidebarCopyTests {
    @Test("no space to free reads None, not a zero-byte size")
    func duplicateSpaceNeverReadsZeroKB() {
        #expect(SettingsTab.duplicateSpaceLabel(reclaimableBytes: 0) == "None")
        #expect(SettingsTab.duplicateSpaceLabel(reclaimableBytes: 12_000_000).contains("MB"))
    }

    @Test("Settings and Cleanup print the same byte total the same way")
    func duplicateSpaceMatchesCleanup() {
        let bytes: Int64 = 22_100_000
        #expect(SettingsTab.duplicateSpaceLabel(reclaimableBytes: bytes) == ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file))
    }

    @Test("Locations says the helper is down instead of claiming there are no locations")
    func locationsEmptyStates() {
        #expect(LocationText.emptyMessage(helperDown: true, helperStarting: false).contains("helper isn't running"))
        #expect(LocationText.emptyMessage(helperDown: false, helperStarting: true).contains("starting"))
        #expect(LocationText.emptyMessage(helperDown: false, helperStarting: false).hasPrefix("No locations yet"))
    }

    @Test("the Documents model row does not claim the Neural Engine")
    func documentsModelRunsOnTheCPU() {
        let technical = ModelInstallCard.bge.rows.compactMap(\.technical).joined(separator: " ")
        #expect(!technical.contains("Neural Engine"))
        #expect(technical.contains("CPU"))
    }

    @Test("a memory refusal is not reported as a model that needs reinstalling")
    func memoryRefusalBanner() {
        let message = "Could not load Qwen3-VL 8B (4-bit): Available memory is too low to load this model safely."
        #expect(Sidebar.errorTitle("deep_load_failed", message: message) == "This Mac is short on memory")
        #expect(!Sidebar.errorText(kind: "deep_load_failed", message: message).contains("Settings"))
        #expect(Sidebar.errorTitle("deep_load_failed", message: "Could not load Qwen3-VL 8B: broken weights") == "An AI model didn't load")
        #expect(Sidebar.errorText(kind: "deep_load_failed", message: "broken weights").contains("Open Settings"))
    }

    @Test("an unnamed person keeps the grid's Person N on the card")
    func unnamedPersonTitle() {
        #expect(PersonCardText.sheetTitle(isUnknown: false, nameParts: ["", " ", "", "", ""], personID: 3) == "Person 3")
        #expect(PersonCardText.sheetTitle(isUnknown: false, nameParts: ["Uncle", "Ben", "", "", ""], personID: 3) == "Uncle Ben")
        #expect(PersonCardText.sheetTitle(isUnknown: true, nameParts: [], personID: 3) == "Unknown person")
    }
}
