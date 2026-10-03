import Foundation

enum ModelMemoryAdmission {
    static func maximumWorkingSetMB(totalMB: UInt64) -> UInt64 {
        let reserve = min(8192, max(2048, totalMB / 4))
        return totalMB > reserve ? totalMB - reserve : 0
    }

    static func minimumFreeMemoryMB(totalMB: UInt64) -> UInt64 {
        min(2048, max(512, totalMB / 16))
    }

    static func rejection(totalMB: UInt64, availableMB: UInt64, requestedMB: UInt64) -> String? {
        guard totalMB > 0, availableMB > 0, requestedMB > 0 else { return "Memory availability could not be established. Retry after checking system resources." }
        guard requestedMB <= maximumWorkingSetMB(totalMB: totalMB) else { return "This model exceeds the memory budget after reserving space for the system and app. Choose a smaller model." }
        let freeFloor = minimumFreeMemoryMB(totalMB: totalMB)
        let available = min(availableMB, totalMB)
        guard available >= freeFloor, requestedMB <= available - freeFloor else { return "Available memory is too low to load this model safely. Finish other work or choose a smaller model." }
        return nil
    }
}
