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
                    .font(Theme.Font.body(12, .semibold))
                Spacer()
                if let eta = state.totalEtaSeconds, eta > 0 {
                    Text(DurationText.short(eta))
                        .font(Theme.Font.body(11).monospacedDigit())
                        .foregroundStyle(Theme.gold)
                }
            }
            ForEach(state.pending) { job in
                HStack(spacing: 6) {
                    Image(systemName: icon(for: job.category))
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.text3)
                        .frame(width: 14)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(job.title)
                            .font(Theme.Font.body(12))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        if let eta = job.etaSeconds, eta > 0 {
                            Text("ETA \(DurationText.short(eta))")
                                .font(Theme.Font.body(11).monospacedDigit())
                                .foregroundStyle(Theme.text3)
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
}
