import Foundation

package enum ModelMemoryAdmission {
    package static func maximumWorkingSetMB(totalMB: UInt64) -> UInt64 {
        let reserve = min(8192, max(2048, totalMB / 4))
        return totalMB > reserve ? totalMB - reserve : 0
    }

    package static func minimumFreeMemoryMB(totalMB: UInt64) -> UInt64 {
        min(2048, max(512, totalMB / 16))
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
