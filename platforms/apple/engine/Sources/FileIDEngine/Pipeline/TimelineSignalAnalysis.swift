import Foundation

enum TimelineSignalAnalysis {
    struct Signal: Codable, Sendable, Equatable {
        let seconds: Double
        let changeScore: Double
    }

    static func sampleTimes(
        duration: Double,
        intervalSeconds: Double,
        signals: [Signal],
        minimumChange: Double = 0.025,
        minimumSpacing: Double = 1.5
    ) -> [Double] {
        guard duration.isFinite, duration > 0,
              intervalSeconds.isFinite, intervalSeconds > 0,
              minimumChange.isFinite, minimumChange >= 0,
              minimumSpacing.isFinite, minimumSpacing >= 0 else { return [] }

        let validSignals = signals.filter {
            $0.seconds.isFinite && $0.seconds >= 0 && $0.seconds < duration &&
            $0.changeScore.isFinite && $0.changeScore >= 0 && $0.changeScore <= 1
        }

        var result = [0.0]
        var windowStart = 0.0
        while windowStart < duration {
            let windowEnd = min(duration, windowStart + intervalSeconds)
            let peak = validSignals
                .filter { $0.seconds >= windowStart && $0.seconds < windowEnd && $0.changeScore >= minimumChange }
                .max {
                    if $0.changeScore == $1.changeScore { return $0.seconds > $1.seconds }
                    return $0.changeScore < $1.changeScore
                }

            let next: Double?
            if let peak, peak.seconds - (result.last ?? 0) >= minimumSpacing {
                next = peak.seconds
            } else if windowEnd < duration, windowEnd - (result.last ?? 0) >= minimumSpacing {
                next = windowEnd
            } else {
                next = nil
            }

            if let next { result.append(next) }
            windowStart += intervalSeconds
        }

        return result
    }
}
