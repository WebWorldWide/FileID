import SwiftUI
import FileIDShared

/// Whole-workflow indicator: Scan → Tag → People → Captions → Done.
/// Reads engine signals plus cheap DB counters so it stays accurate
/// across launches when `engine.lastProgress` is nil but the DB
/// already reflects work done in a prior session.
struct PipelineProgress: View {
    let engine: EngineClient
    let store: ReadStore

    enum Stage: Int, CaseIterable, Identifiable {
        case scan = 0, tag, people, captions, done
        var id: Int { rawValue }
        var label: String {
            switch self {
            case .scan:     return "Scan"
            case .tag:      return "Tag"
            case .people:   return "People"
            case .captions: return "Captions"
            case .done:     return "Done"
            }
        }
    }

    /// Where the user is in the workflow right now. Live signals win
    /// over DB-derived state so the bar tracks an in-flight stage.
    private var current: Stage {
        if let p = engine.lastProgress {
            switch p.phase {
            case .discovering: return .scan
            case .tagging:     return .tag
            case .postScan:    return .people
            case .completed, .cancelled, .failed, .idle: break
            }
        }
        if engine.faceClusteringInFlight { return .people }
        if engine.deepAnalyzeInFlight    { return .captions }

        // Nothing in flight — derive from the DB state.
        let scanned   = store.totalFiles > 0
        let clustered = store.totalFacePrints() > 0
        let named     = store.namedPersonCount() > 0
        let captioned = store.totalCaptioned() > 0
        if !scanned   { return .scan }
        if !clustered { return .people }   // clustering still pending
        if !named     { return .people }   // user hasn't named anyone yet
        if !captioned { return .captions } // Deep Analyze still pending
        return .done
    }

    private func state(for s: Stage) -> (filled: Bool, active: Bool) {
        let c = current
        // Done is "filled" only when current = done (everything's complete).
        // Otherwise every stage strictly before the current one is filled,
        // and the current stage itself is active.
        let filled = s.rawValue < c.rawValue || c == .done
        let active = s == c
        return (filled, active)
    }

    var body: some View {
        // 5 equal columns; each column has its dot centered above its
        // label so they always align vertically. Connector segments live
        // in the same column as the dot — left half + right half — so
        // they meet between adjacent dots without offsetting them.
        let stages = Stage.allCases
        HStack(spacing: 0) {
            ForEach(Array(stages.enumerated()), id: \.element.id) { idx, s in
                let st = state(for: s)
                let prevFilled = idx > 0 ? state(for: stages[idx - 1]).filled : false
                VStack(spacing: 4) {
                    ZStack {
                        // Left connector — only when not the first dot.
                        // Filled when the PREVIOUS stage is filled (the
                        // segment "leads into" this dot from the left).
                        if idx > 0 {
                            HStack(spacing: 0) {
                                Rectangle()
                                    .fill(prevFilled ? Theme.gold : Color.white.opacity(0.10))
                                    .frame(height: 1)
                                Spacer(minLength: 0)
                            }
                        }
                        // Right connector — only when not the last dot.
                        if idx < stages.count - 1 {
                            HStack(spacing: 0) {
                                Spacer(minLength: 0)
                                Rectangle()
                                    .fill(st.filled ? Theme.gold : Color.white.opacity(0.10))
                                    .frame(height: 1)
                            }
                        }
                        dotCell(state: st)
                    }
                    .frame(height: 14)
                    Text(s.label)
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(st.active ? Theme.gold
                                          : (st.filled ? Color.primary : Color.secondary))
                }
                .frame(maxWidth: .infinity)
            }
        }
        .padding(.horizontal, 4)
    }

    @ViewBuilder
    private func dotCell(state st: (filled: Bool, active: Bool)) -> some View {
        // Active dot grows by frame, not scale, to keep layout stable.
        let size: CGFloat = st.active ? 12 : 8
        let fill: Color = st.filled
            ? Theme.gold
            : (st.active ? Theme.gold.opacity(0.6) : Color.white.opacity(0.12))
        let stroke: Color = st.active ? Theme.gold : Color.white.opacity(0.18)
        Circle()
            .fill(fill)
            .frame(width: size, height: size)
            .overlay(Circle().stroke(stroke, lineWidth: st.active ? 1.5 : 1))
            .shadow(color: st.active ? Theme.gold.opacity(0.55) : .clear,
                    radius: st.active ? 4 : 0)
    }
}
