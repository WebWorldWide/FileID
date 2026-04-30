import SwiftUI
import FileIDShared

/// Live scan controls. Renders one of three states: pre-scan,
/// in-flight, or terminal (done/cancelled/failed).
struct ProcessingControl: View {
    let engine: EngineClient
    let store: ReadStore
    let pickedURL: URL
    let changePickedURL: (URL?) -> Void

    /// Optimistic flag set when Start is clicked, cleared on the first
    /// engine phase event. Suppresses spam-click double-starts.
    @State private var startRequested = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            // Whole-pipeline indicator is always visible — even when
            // nothing is in flight — so the user can see at a glance
            // where they are in the workflow (e.g., post-scan they
            // know "now go name people, then run Deep Analyze").
            PipelineProgress(engine: engine, store: store)
                .padding(.vertical, 2)

            if let p = engine.lastProgress, p.phase != .idle {
                liveOrDone(p)
            } else {
                preScan
            }
        }
        .padding(.vertical, 4)
        .onChange(of: engine.lastProgress?.phase) { _, new in
            if new != nil && new != .idle {
                startRequested = false
            }
        }
    }

    // MARK: Pre-scan

    @ViewBuilder
    private var preScan: some View {
        HStack {
            Image(systemName: startRequested ? "hourglass" : "moon.zzz")
                .foregroundStyle(.secondary)
            Text(startRequested ? "Starting…" : "Idle")
                .font(.caption.bold()).foregroundStyle(.secondary)
            Spacer()
        }
        Text(startRequested
             ? "Sent to engine. The first phase event will appear shortly."
             : "Ready to scan this folder. Click Start when you're ready.")
            .font(.caption2)
            .foregroundStyle(.secondary)
        pillButton("Start Scan", color: Theme.gold, system: "play.fill",
                    filled: true, disabled: startRequested) {
            startRequested = true
            engine.startScan(rootURL: pickedURL)
        }
        .keyboardShortcut("r", modifiers: .command)
    }

    // MARK: Live or terminal

    @ViewBuilder
    private func liveOrDone(_ p: ScanProgress) -> some View {
        HStack(spacing: 8) {
            Image(systemName: phaseIcon(p.phase))
                .font(.callout)
                .foregroundStyle(Theme.gold)
            BadgePill(label: p.phase.rawValue.capitalized)
            Spacer()
        }

        Text(statusText(p))
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(p.phase == .completed ? Theme.gold : Color.primary)
            .fixedSize(horizontal: false, vertical: true)

        if p.phase == .tagging, p.total > 0 {
            ProgressView(value: Double(p.processed), total: Double(max(p.total, 1)))
                .tint(Theme.gold)
            HStack {
                Text("\(p.processed) / \(p.total)")
                    .font(.caption2.monospacedDigit())
                Spacer()
                let pct = Double(p.processed) / Double(max(p.total, 1)) * 100
                Text(String(format: "%.1f%%", pct))
                    .font(.caption2.bold().monospacedDigit())
                    .foregroundStyle(Theme.gold)
            }
        } else if p.phase == .discovering {
            ProgressView().controlSize(.small)
        }

        // Stats grouped on a subtle recessed card so the row of small
        // numbers reads as a unit. Without this they were a wall of
        // monospaced text that competed with the headline above.
        VStack(alignment: .leading, spacing: 6) {
            statRow(icon: "magnifyingglass",
                     label: "\(p.discovered.formatted()) found",
                     trailing: (p.etaSeconds.flatMap { $0 > 0 ? "\(formatETA($0)) left" : nil }),
                     trailingTint: Theme.gold)
            if p.processed > 0 {
                statRow(icon: "tag",
                         label: "\(p.processed.formatted()) tagged",
                         trailing: String(format: "%.1f/s", p.filesPerSecond),
                         trailingTint: Theme.gold)
            }
            statRow(icon: "memorychip",
                     label: "\(p.residentMB) MB used",
                     labelTint: p.residentMB > 1200 ? .orange : .secondary,
                     trailing: "\(p.availableMB) MB free",
                     trailingTint: .secondary)
            if p.failed > 0 {
                statRow(icon: "exclamationmark.triangle",
                         label: "\(p.failed) failed",
                         labelTint: .red,
                         trailing: nil,
                         trailingTint: .secondary)
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(Color.white.opacity(0.04))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(Color.white.opacity(0.06), lineWidth: 1)
        )

        if engine.isPaused {
            HStack(spacing: 6) {
                Image(systemName: "pause.circle.fill").foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Paused")
                        .font(.caption.bold())
                        .foregroundStyle(.orange)
                    Text("Workers idle. Click Resume to continue or Cancel to stop.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            .padding(8)
            .background(Color.orange.opacity(0.10))
            .overlay(
                RoundedRectangle(cornerRadius: 6).stroke(Color.orange.opacity(0.5), lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: 6))
        }
        // Lid-closed safety hint moved to Settings → Engine info to
        // keep the sidebar focused on actionable scan controls.
        HStack(spacing: 8) {
            if engine.isPaused {
                pillButton("Resume", color: .green, system: "play.fill", filled: true) {
                    engine.resume()
                }
                pillButton("Cancel", color: .red, system: "xmark.circle.fill") {
                    engine.cancel()
                }
            } else if p.phase == .tagging || p.phase == .discovering || p.phase == .postScan {
                pillButton("Pause", color: .orange, system: "pause.fill") {
                    engine.pause()
                }
                pillButton("Cancel", color: .red, system: "xmark.circle.fill") {
                    engine.cancel()
                }
            } else {
                pillButton("Rescan", color: Theme.gold, system: "arrow.counterclockwise", filled: true) {
                    engine.startScan(rootURL: pickedURL)
                }
            }
        }
    }

    @ViewBuilder
    private func statRow(icon: String, label: String,
                          labelTint: Color = .secondary,
                          trailing: String?,
                          trailingTint: Color) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .font(.caption2)
                .foregroundStyle(labelTint)
                .frame(width: 14, alignment: .leading)
            Text(label)
                .font(.caption2.monospacedDigit())
                .foregroundStyle(labelTint)
            Spacer(minLength: 8)
            if let trailing {
                Text(trailing)
                    .font(.caption2.bold().monospacedDigit())
                    .foregroundStyle(trailingTint)
            }
        }
    }

    private func phaseIcon(_ phase: ScanPhase) -> String {
        switch phase {
        case .idle:        return "moon.zzz"
        case .discovering: return "magnifyingglass"
        case .tagging:     return "brain.head.profile"
        case .postScan:    return "wand.and.stars"
        case .completed:   return "checkmark.seal.fill"
        case .cancelled:   return "xmark.octagon"
        case .failed:      return "exclamationmark.triangle"
        }
    }

    private func statusText(_ p: ScanProgress) -> String {
        switch p.phase {
        case .idle:        return "Idle"
        case .discovering: return "Discovering files…"
        case .tagging:     return "Tagging files…"
        case .postScan:    return "Post-scan…"
        case .completed:   return "Done — \(p.processed) of \(p.total) tagged"
        case .cancelled:   return "Cancelled — \(p.processed) of \(p.total) tagged"
        case .failed:      return "Failed"
        }
    }

    private func formatETA(_ seconds: Double) -> String {
        let s = Int(seconds.rounded())
        let h = s / 3600
        let m = (s % 3600) / 60
        let sec = s % 60
        if h > 0 { return String(format: "%dh %dm", h, m) }
        if m > 0 { return String(format: "%dm %ds", m, sec) }
        return "\(sec)s"
    }

    @ViewBuilder
    private func pillButton(_ title: String, color: Color, system: String,
                             filled: Bool = false,
                             disabled: Bool = false,
                             action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: system)
                .font(.caption.bold())
                .foregroundStyle(disabled ? color.opacity(0.5)
                                  : (filled ? Color.black : color))
                .frame(maxWidth: .infinity)
                .padding(.vertical, 6)
                .background(filled ? color.opacity(disabled ? 0.4 : 1.0)
                                   : color.opacity(0.18))
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(color.opacity(filled ? 1.0 : 0.6), lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.plain)
        .disabled(disabled)
    }
}
