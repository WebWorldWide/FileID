import Foundation

/// Chat summaries waiting behind a Describe batch. The batch runs them between files, so
/// a question is answered after the current file instead of after the whole batch. There is
/// one resident model, so generations still take turns. A parked summary also sits in the
/// job queue; whichever reaches it first removes it here and runs it, so it runs once.
actor ChatYield {
    static let shared = ChatYield()

    private var waiting: [(id: String, run: @Sendable (Bool) async -> Void)] = []

    func park(id: String, _ run: @escaping @Sendable (Bool) async -> Void) {
        waiting.append((id, run))
    }

    /// True if `id` was still parked (the caller now owns running it).
    func remove(id: String) -> Bool {
        guard let index = waiting.firstIndex(where: { $0.id == id }) else { return false }
        waiting.remove(at: index)
        return true
    }

    var isEmpty: Bool { waiting.isEmpty }

    func drain() async {
        while !waiting.isEmpty {
            let next = waiting.removeFirst()
            await next.run(true)
        }
    }
}
