// F-C6-013: Restructure.apply was a silent, unstoppable serial loop. The
// apply-progress throttle is a pure function, so its cadence is unit-assertable
// here (a 100k-move apply emits ~total/interval log lines, not zero or one per
// move). The cancellation poll + throttled logging inside `apply` itself are
// CI/Mac-verified via `swift test` (no Xcode in this dev env); only the pure
// cadence is asserted as a unit.
import Testing
@testable import FileIDEngine

@Suite("Restructure apply progress throttle (F-C6-013)")
struct RestructureApplyProgressTests {
    @Test("Emits on first, last, and every interval-th move; silent otherwise")
    func cadence() {
        // Never on the zeroth processed item or with a zero interval.
        #expect(Restructure.shouldEmitApplyProgress(processed: 0, total: 1000, interval: 500) == false)
        #expect(Restructure.shouldEmitApplyProgress(processed: 500, total: 1000, interval: 0) == false)
        // First move (immediate feedback), every `interval`, and the last move.
        #expect(Restructure.shouldEmitApplyProgress(processed: 1, total: 1000, interval: 500) == true)
        #expect(Restructure.shouldEmitApplyProgress(processed: 500, total: 1000, interval: 500) == true)
        #expect(Restructure.shouldEmitApplyProgress(processed: 1000, total: 1000, interval: 500) == true)
        // Silent on in-between indices (so 100k moves → ~200 lines, not 100k).
        #expect(Restructure.shouldEmitApplyProgress(processed: 2, total: 1000, interval: 500) == false)
        #expect(Restructure.shouldEmitApplyProgress(processed: 499, total: 1000, interval: 500) == false)
        #expect(Restructure.shouldEmitApplyProgress(processed: 501, total: 1000, interval: 500) == false)
    }
}
