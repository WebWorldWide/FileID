import FileIDModelInfo
import Foundation

package enum ModelMemoryAdmission {
    package static func maximumWorkingSetMB(totalMB: UInt64) -> UInt64 {
        let reserve = ModelMemoryRule.reserveMB(totalMB: totalMB)
        return totalMB > reserve ? totalMB - reserve : 0
    }

    package static func minimumFreeMemoryMB(totalMB: UInt64) -> UInt64 {
        ModelMemoryRule.freeFloorMB(totalMB: totalMB)
    }

    static func rejection(totalMB: UInt64, availableMB: UInt64, requestedMB: UInt64) -> String? {
        guard totalMB > 0, availableMB > 0, requestedMB > 0 else { return "FileID couldn't tell how much memory is free. Try again in a moment." }
        guard requestedMB <= maximumWorkingSetMB(totalMB: totalMB) else { return "This model needs more memory than this Mac has. Pick a smaller model in Settings." }
        let freeFloor = minimumFreeMemoryMB(totalMB: totalMB)
        let available = min(availableMB, totalMB)
        guard available >= freeFloor, requestedMB <= available - freeFloor else { return "This model needs more memory than is free right now. Close other apps or pick a smaller model in Settings." }
        return nil
    }
}
