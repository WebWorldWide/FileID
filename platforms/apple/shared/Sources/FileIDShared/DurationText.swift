import Foundation

public enum DurationText {
    /// "2h 5m", "4m 12s" or "9s"; capped at 99h 59m.
    public static func short(_ seconds: Double) -> String {
        guard seconds.isFinite else { return "—" }
        let s = max(0, Int(min(seconds, 359_940).rounded()))
        let h = s / 3600, m = (s % 3600) / 60, sec = s % 60
        if h > 0 { return "\(h)h \(m)m" }
        if m > 0 { return "\(m)m \(sec)s" }
        return "\(sec)s"
    }

    /// Playback position: "1:02:03" or "4:05".
    public static func clock(_ seconds: Double) -> String {
        let (h, m, s) = parts(seconds)
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }

    /// VoiceOver form of `clock`: "1 hour 2 minutes 3 seconds".
    public static func spoken(_ seconds: Double) -> String {
        let (h, m, s) = parts(seconds)
        var words: [String] = []
        if h > 0 { words.append("\(h) hour\(h == 1 ? "" : "s")") }
        if m > 0 { words.append("\(m) minute\(m == 1 ? "" : "s")") }
        words.append("\(s) second\(s == 1 ? "" : "s")")
        return words.joined(separator: " ")
    }

    private static func parts(_ seconds: Double) -> (Int, Int, Int) {
        let total = Int(max(0, seconds.isFinite ? seconds : 0).rounded(.down))
        return (total / 3600, (total % 3600) / 60, total % 60)
    }
}
