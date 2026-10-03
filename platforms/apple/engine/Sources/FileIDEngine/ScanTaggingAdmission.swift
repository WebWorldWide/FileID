import Foundation

enum ScanTaggingAdmission {
    static func withBackgroundCPU<T: Sendable>(
        scheduler: ResourceScheduler = .shared,
        isCancelled: @escaping @Sendable () -> Bool,
        operation: @escaping @Sendable (Double) async throws -> T
    ) async throws -> T {
        let requestedAt = ProcessInfo.processInfo.systemUptime
        return try await scheduler.withReservation(
            ResourceScheduler.Demand(cpuUnits: 1),
            priority: .background,
            waitForCapacity: true,
            isCancelled: isCancelled
        ) {
            let waitMs = max(0, ProcessInfo.processInfo.systemUptime - requestedAt) * 1000
            return try await operation(waitMs)
        }
    }
}
