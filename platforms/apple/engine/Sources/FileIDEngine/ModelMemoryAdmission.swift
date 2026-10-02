import Foundation

enum ModelMemoryAdmission {
    static func rejection(totalMB: UInt64, availableMB: UInt64, requestedMB: UInt64) -> String? {
        guard totalMB > 0, availableMB > 0, requestedMB > 0 else { return "Memory availability could not be established. Retry after checking system resources." }
        let reserve = min(8192, max(2048, totalMB / 4))
        guard totalMB > reserve, requestedMB <= totalMB - reserve else { return "This model exceeds the memory budget after reserving space for the system and app. Choose a smaller model." }
        let freeFloor = min(2048, max(512, totalMB / 16))
        let available = min(availableMB, totalMB)
        guard available >= freeFloor, requestedMB <= available - freeFloor else { return "Available memory is too low to load this model safely. Finish other work or choose a smaller model." }
        return nil
    }
}
