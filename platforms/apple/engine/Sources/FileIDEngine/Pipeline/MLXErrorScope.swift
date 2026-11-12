import FileIDVLMCore
import Foundation
import MLX

/// Runs `body` with MLX errors (a Metal GPU timeout, an allocation failure)
/// thrown as `MLXError` when it returns, instead of mlx-swift's default handler
/// aborting the whole engine. MLX keeps going after an error, so the scope is
/// one file or one request: its result is discarded, the job continues.
func catchingMLX<R>(_ body: () async throws -> R) async throws -> R {
    try await withError { try await body() }
}

extension DeepAnalyze {
    /// `runCancellableAnalysis` inside an MLX error scope. An MLX error becomes
    /// an "Inference failed" result, which callers count as a failed file and
    /// never store.
    nonisolated func guardedAnalysis(
        _ operation: @escaping @Sendable () async -> AnalysisResult
    ) async -> AnalysisResult {
        do {
            return try await catchingMLX { await self.runCancellableAnalysis(operation) }
        } catch {
            JSONLog.shared.error(ev: "mlx_error", error: error.localizedDescription)
            return AnalysisResult.make(description: "Inference failed: \(error.localizedDescription)", proposedName: nil)
        }
    }
}
