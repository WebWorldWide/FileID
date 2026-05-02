import SwiftUI
import FileIDShared

struct QueueListView: View {
    let state: QueueState

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Image(systemName: "list.bullet.rectangle")
                    .foregroundStyle(Theme.gold)
                Text("\(state.pending.count) waiting")
                    .font(.caption.bold())
                Spacer()
                if let eta = state.totalEtaSeconds, eta > 0 {
                    Text(formatETA(eta))
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(Theme.gold)
                }
            }
            ForEach(state.pending) { job in
                HStack(spacing: 6) {
                    Image(systemName: icon(for: job.category))
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .frame(width: 14)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(job.title)
                            .font(.caption)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        if let eta = job.etaSeconds, eta > 0 {
                            Text("ETA \(formatETA(eta))")
                                .font(.caption2.monospacedDigit())
                                .foregroundStyle(.tertiary)
                        }
                    }
                    Spacer()
                }
            }
        }
    }

    private func icon(for c: JobCategory) -> String {
        switch c {
        case .scan:         return "magnifyingglass"
        case .faceCluster:  return "person.2.crop.square.stack"
        case .deepAnalyze:  return "sparkles"
        }
    }

    private func formatETA(_ seconds: Double) -> String {
        let s = max(0, Int(seconds.rounded()))
        let h = s / 3600
        let m = (s % 3600) / 60
        let sec = s % 60
        if h > 0 { return String(format: "%dh %dm", h, m) }
        if m > 0 { return String(format: "%dm %ds", m, sec) }
        return "\(sec)s"
    }
}
