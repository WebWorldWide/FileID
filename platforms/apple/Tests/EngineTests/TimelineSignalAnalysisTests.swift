import Testing
@testable import FileIDEngine

struct TimelineSignalAnalysisTests {
    @Test func preservesUniformCoverageWhenNoStrongChangeExists() {
        #expect(TimelineSignalAnalysis.sampleTimes(duration: 30, intervalSeconds: 10, signals: []) == [0, 10, 20])
    }

    @Test func selectsTheStrongestVisualChangeInsideEachSamplingWindow() {
        let signals = [
            TimelineSignalAnalysis.Signal(seconds: 4, changeScore: 0.2),
            TimelineSignalAnalysis.Signal(seconds: 8, changeScore: 0.4),
            TimelineSignalAnalysis.Signal(seconds: 12, changeScore: 0.1),
            TimelineSignalAnalysis.Signal(seconds: 18, changeScore: 0.3),
            TimelineSignalAnalysis.Signal(seconds: 27, changeScore: 0.5),
        ]

        #expect(TimelineSignalAnalysis.sampleTimes(duration: 30, intervalSeconds: 10, signals: signals) == [0, 8, 18, 27])
    }

    @Test func fallsBackToWindowBoundaryForWeakChangesAndRejectsInvalidSignals() {
        let signals = [
            TimelineSignalAnalysis.Signal(seconds: 4, changeScore: 0.015),
            TimelineSignalAnalysis.Signal(seconds: .nan, changeScore: 1),
            TimelineSignalAnalysis.Signal(seconds: 15, changeScore: 2),
        ]

        #expect(TimelineSignalAnalysis.sampleTimes(duration: 30, intervalSeconds: 10, signals: signals) == [0, 10, 20])
    }

    @Test func keepsBaselineAndRejectsNearDuplicateCandidate() {
        let signals = [TimelineSignalAnalysis.Signal(seconds: 0.5, changeScore: 0.9)]

        #expect(TimelineSignalAnalysis.sampleTimes(duration: 12, intervalSeconds: 10, signals: signals) == [0, 10])
    }

    @Test func invalidDurationAndIntervalProduceNoSamples() {
        #expect(TimelineSignalAnalysis.sampleTimes(duration: .infinity, intervalSeconds: 10, signals: []) == [])
        #expect(TimelineSignalAnalysis.sampleTimes(duration: 10, intervalSeconds: 0, signals: []) == [])
    }
}
