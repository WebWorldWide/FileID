import Foundation

actor ExclusiveResourceGate {
    private var owner: UUID?
    private var waiting: [(UUID, CheckedContinuation<UUID, Error>)] = []

    func acquire() async throws -> UUID {
        try Task.checkCancellation()
        let id = UUID()
        if owner == nil { owner = id; return id }
        let token: UUID = try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                if Task.isCancelled { continuation.resume(throwing: CancellationError()) }
                else { waiting.append((id, continuation)) }
            }
        } onCancel: {
            Task { await self.cancel(id) }
        }
        if Task.isCancelled { release(token); throw CancellationError() }
        return token
    }

    func release(_ token: UUID) {
        guard owner == token else { return }
        owner = nil
        if !waiting.isEmpty {
            let (id, continuation) = waiting.removeFirst()
            owner = id
            continuation.resume(returning: id)
        }
    }

    func snapshot() -> (occupied: Bool, waiting: Int) { (owner != nil, waiting.count) }

    private func cancel(_ id: UUID) {
        // A completed handoff owns resources until its caller unwinds.
        if owner == id { return }
        if let index = waiting.firstIndex(where: { $0.0 == id }) {
            waiting.remove(at: index).1.resume(throwing: CancellationError())
        }
    }
}
