import Foundation

actor ResourceScheduler {
    struct MemoryLease: Hashable, Sendable {
        fileprivate let id: UUID
    }

    struct Demand: Sendable, Equatable {
        let memoryMB: UInt64
        let cpuUnits: Int
        let ioUnits: Int

        init(memoryMB: UInt64 = 0, cpuUnits: Int = 0, ioUnits: Int = 0) {
            self.memoryMB = memoryMB
            self.cpuUnits = cpuUnits
            self.ioUnits = ioUnits
        }
    }

    enum Priority: Int, Sendable {
        case background
        case interactive
    }

    struct Snapshot: Sendable, Equatable {
        let reservedMemoryMB: UInt64
        let reservedCPUUnits: Int
        let reservedIOUnits: Int
        let activeReservations: Int
        let waitingCount: Int
    }

    enum AdmissionError: Error, LocalizedError, Equatable {
        case invalidMemoryEstimate
        case exceedsSystemBudget
        case exceedsResourceCapacity
        case insufficientAvailableMemory
        case insufficientResources

        var errorDescription: String? {
            switch self {
            case .invalidMemoryEstimate:
                "Memory availability or the requested allocation could not be established."
            case .exceedsSystemBudget:
                "This operation exceeds the memory budget reserved for the system and app. Choose a smaller model or library."
            case .exceedsResourceCapacity:
                "This operation exceeds the available local processing or storage capacity."
            case .insufficientAvailableMemory:
                "Available memory is too low to run this operation safely. Finish other work or try again later."
            case .insufficientResources:
                "The local processing capacity is busy. Try again after current work finishes."
            }
        }
    }

    private struct Waiter {
        let id: UUID
        let demand: Demand
        let priority: Priority
        let sequence: UInt64
        let continuation: CheckedContinuation<MemoryLease, Error>
    }

    private struct ActiveReservation {
        let demand: Demand
        let priority: Priority
    }

    static let shared = ResourceScheduler(memory: {
        (ProcessInfo.processInfo.physicalMemory / 1_048_576,
         UInt64(max(0, Hardware.availableMemoryMB())))
    })

    private let memory: @Sendable () -> (total: UInt64, available: UInt64)
    private let cpuCapacity: Int
    private let ioCapacity: Int
    private var reservations: [MemoryLease: ActiveReservation] = [:]
    private var waiters: [Waiter] = []
    private var nextSequence: UInt64 = 0

    init(memory: @escaping @Sendable () -> (total: UInt64, available: UInt64),
         cpuCapacity: Int = max(1, ProcessInfo.processInfo.activeProcessorCount - 2),
         ioCapacity: Int = 2) {
        self.memory = memory
        self.cpuCapacity = max(1, cpuCapacity)
        self.ioCapacity = max(1, ioCapacity)
    }

    func reserveMemory(_ requestedMB: UInt64, priority: Priority) async throws -> MemoryLease {
        try await reserve(Demand(memoryMB: requestedMB), priority: priority)
    }

    func reserve(_ demand: Demand, priority: Priority) async throws -> MemoryLease {
        let id = UUID()
        try validate(demand)
        if waiters.isEmpty, canGrant(demand) {
            let lease = MemoryLease(id: id)
            reservations[lease] = ActiveReservation(demand: demand, priority: priority)
            return lease
        }
        if priority == .background, hasInteractiveReservation, !canGrant(demand) {
            throw admissionError(demand)
        }
        if reservations.isEmpty {
            throw admissionError(demand)
        }

        let lease: MemoryLease = try await withTaskCancellationHandler(operation: {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<MemoryLease, Error>) in
                if Task.isCancelled {
                    continuation.resume(throwing: CancellationError())
                    return
                }
                nextSequence &+= 1
                let waiter = Waiter(id: id, demand: demand, priority: priority,
                                    sequence: nextSequence, continuation: continuation)
                let insertion = waiters.firstIndex {
                    $0.priority.rawValue < priority.rawValue
                        || ($0.priority == priority && $0.sequence > waiter.sequence)
                } ?? waiters.endIndex
                waiters.insert(waiter, at: insertion)
                drainWaiters()
            }
        }, onCancel: {
            Task { await self.cancelWaiter(id) }
        })
        if Task.isCancelled {
            release(lease)
            throw CancellationError()
        }
        return lease
    }

    func withReservation<T: Sendable>(
        _ demand: Demand,
        priority: Priority,
        waitForCapacity: Bool = false,
        isCancelled: @Sendable () -> Bool = { Task.isCancelled },
        operation: @Sendable () async throws -> T
    ) async throws -> T {
        let lease: MemoryLease
        while true {
            try Task.checkCancellation()
            if isCancelled() { throw CancellationError() }
            do {
                if waitForCapacity {
                    lease = try reserveImmediately(demand, priority: priority)
                } else {
                    lease = try await reserve(demand, priority: priority)
                }
                break
            } catch AdmissionError.insufficientResources where waitForCapacity {
                try await Task.sleep(for: .milliseconds(100))
            }
        }

        defer { release(lease) }
        try Task.checkCancellation()
        if isCancelled() { throw CancellationError() }
        return try await operation()
    }

    private func reserveImmediately(_ demand: Demand, priority: Priority) throws -> MemoryLease {
        try validate(demand)
        guard waiters.isEmpty, canGrant(demand) else { throw admissionError(demand) }
        let lease = MemoryLease(id: UUID())
        reservations[lease] = ActiveReservation(demand: demand, priority: priority)
        return lease
    }

    func release(_ lease: MemoryLease) {
        guard reservations.removeValue(forKey: lease) != nil else { return }
        drainWaiters()
    }

    func snapshot() -> Snapshot {
        Snapshot(reservedMemoryMB: reservations.values.map { $0.demand.memoryMB }.reduce(0, saturatingAdd),
                 reservedCPUUnits: reservations.values.map { $0.demand.cpuUnits }.reduce(0, saturatingAdd),
                 reservedIOUnits: reservations.values.map { $0.demand.ioUnits }.reduce(0, saturatingAdd),
                 activeReservations: reservations.count,
                 waitingCount: waiters.count)
    }

    func retainMemory(_ lease: MemoryLease) {
        guard let active = reservations[lease] else { return }
        reservations[lease] = ActiveReservation(demand: Demand(memoryMB: active.demand.memoryMB),
                                                priority: active.priority)
        drainWaiters()
    }

    private func validate(_ demand: Demand) throws {
        let (totalMB, availableMB) = memory()
        guard demand.memoryMB > 0 || demand.cpuUnits > 0 || demand.ioUnits > 0,
              demand.cpuUnits >= 0, demand.ioUnits >= 0 else {
            throw AdmissionError.invalidMemoryEstimate
        }
        guard demand.cpuUnits <= cpuCapacity, demand.ioUnits <= ioCapacity else {
            throw AdmissionError.exceedsResourceCapacity
        }
        if demand.memoryMB > 0 {
            guard totalMB > 0, availableMB > 0 else { throw AdmissionError.invalidMemoryEstimate }
            guard demand.memoryMB <= ModelMemoryAdmission.maximumWorkingSetMB(totalMB: totalMB) else {
                throw AdmissionError.exceedsSystemBudget
            }
        }
    }

    private func canGrant(_ demand: Demand) -> Bool {
        let reservedCPU = reservations.values.map { $0.demand.cpuUnits }.reduce(0, saturatingAdd)
        let reservedIO = reservations.values.map { $0.demand.ioUnits }.reduce(0, saturatingAdd)
        guard demand.cpuUnits <= cpuCapacity - min(cpuCapacity, reservedCPU),
              demand.ioUnits <= ioCapacity - min(ioCapacity, reservedIO) else { return false }
        guard demand.memoryMB > 0 else { return true }

        let (totalMB, availableMB) = memory()
        guard totalMB > 0, availableMB > 0 else { return false }
        let workingSet = ModelMemoryAdmission.maximumWorkingSetMB(totalMB: totalMB)
        let reserved = reservations.values.map { $0.demand.memoryMB }.reduce(0, saturatingAdd)
        let budgetRemaining = workingSet > reserved ? workingSet - reserved : 0
        let freeFloor = ModelMemoryAdmission.minimumFreeMemoryMB(totalMB: totalMB)
        let available = min(totalMB, availableMB)
        let operatingSystemRemaining = available > freeFloor ? available - freeFloor : 0
        return demand.memoryMB <= min(budgetRemaining, operatingSystemRemaining)
    }

    private func admissionError(_ demand: Demand) -> AdmissionError {
        let reservedCPU = reservations.values.map { $0.demand.cpuUnits }.reduce(0, saturatingAdd)
        let reservedIO = reservations.values.map { $0.demand.ioUnits }.reduce(0, saturatingAdd)
        if demand.cpuUnits > cpuCapacity - min(cpuCapacity, reservedCPU)
            || demand.ioUnits > ioCapacity - min(ioCapacity, reservedIO) {
            return .insufficientResources
        }
        if demand.memoryMB == 0 {
            return .insufficientResources
        }
        let (totalMB, availableMB) = memory()
        guard totalMB > 0, availableMB > 0 else { return .invalidMemoryEstimate }
        guard demand.memoryMB <= ModelMemoryAdmission.maximumWorkingSetMB(totalMB: totalMB) else {
            return .exceedsSystemBudget
        }
        return .insufficientAvailableMemory
    }

    private func drainWaiters() {
        while let waiter = waiters.first {
            if canGrant(waiter.demand) {
                waiters.removeFirst()
                let lease = MemoryLease(id: waiter.id)
                reservations[lease] = ActiveReservation(demand: waiter.demand, priority: waiter.priority)
                waiter.continuation.resume(returning: lease)
                continue
            }
            if waiter.priority == .background, hasInteractiveReservation {
                waiters.removeFirst()
                waiter.continuation.resume(throwing: admissionError(waiter.demand))
                continue
            }
            guard reservations.isEmpty else { return }
            waiters.removeFirst()
            waiter.continuation.resume(throwing: admissionError(waiter.demand))
        }
    }

    private func cancelWaiter(_ id: UUID) {
        guard let index = waiters.firstIndex(where: { $0.id == id }) else { return }
        let waiter = waiters.remove(at: index)
        waiter.continuation.resume(throwing: CancellationError())
        drainWaiters()
    }

    private var hasInteractiveReservation: Bool {
        reservations.values.contains { $0.priority == .interactive }
    }

    private func saturatingAdd(_ partial: UInt64, _ next: UInt64) -> UInt64 {
        let (sum, overflow) = partial.addingReportingOverflow(next)
        return overflow ? .max : sum
    }

    private func saturatingAdd(_ partial: Int, _ next: Int) -> Int {
        let (sum, overflow) = partial.addingReportingOverflow(next)
        return overflow ? .max : sum
    }
}
