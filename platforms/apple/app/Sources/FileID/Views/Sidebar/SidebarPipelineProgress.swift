import SwiftUI
import FileIDShared

struct PipelineTrackGeometry {
    let width: CGFloat
    let stageCount: Int

    var columnWidth: CGFloat {
        guard stageCount > 0 else { return 0 }
        return width / CGFloat(stageCount)
    }

    func center(for index: Int) -> CGFloat {
        guard stageCount > 0 else { return 0 }
        let bounded = min(max(index, 0), stageCount - 1)
        return columnWidth * (CGFloat(bounded) + 0.5)
    }

    func filledWidth(through index: Int) -> CGFloat {
        max(0, center(for: index) - center(for: 0))
    }
}

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
            // postScan is the SCAN finalizing (orphan sweep / stats), not People
            // clustering — keep the indicator in the scan region so the fill doesn't
            // jump to the People dot (the halfway mark) before clustering begins.
            // It advances to People below once face clustering is actually in flight.
            case .postScan:    return .tag
            case .completed, .cancelled, .failed, .idle: break
            }
        }
        if engine.faceClusteringInFlight { return .people }
        if engine.deepAnalyzeInFlight    { return .captions }
        // A paused scan is mid-flight, not done — never let the DB-derived branch below
        // advance to People/Captions (which would push the fill to/past the halfway dot)
        // just because the partial scan already wrote some rows.
        if engine.isPaused { return .tag }

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

    private func state(for s: Stage, _ c: Stage) -> (filled: Bool, active: Bool) {
        // Done is "filled" only when current = done (everything's complete).
        // Otherwise every stage strictly before the current one is filled,
        // and the current stage itself is active.
        let filled = s.rawValue < c.rawValue || c == .done
        let active = s == c
        return (filled, active)
    }

    var body: some View {
        let stages = Stage.allCases
        let cur = current
        VStack(spacing: 4) {
            GeometryReader { proxy in
                let geometry = PipelineTrackGeometry(
                    width: proxy.size.width,
                    stageCount: stages.count
                )
                let start = geometry.center(for: 0)
                let trackWidth = geometry.filledWidth(through: stages.count - 1)
                let fillWidth = geometry.filledWidth(through: cur.rawValue)

                ZStack(alignment: .leading) {
                    Rectangle()
                        .fill(Color.white.opacity(0.10))
                        .frame(width: trackWidth, height: 1)
                        .offset(x: start)
                    Rectangle()
                        .fill(Theme.gold)
                        .frame(width: fillWidth, height: 1)
                        .offset(x: start)
                        .animation(.easeOut(duration: 0.22), value: fillWidth)
                    HStack(spacing: 0) {
                        ForEach(stages) { stage in
                            dotCell(state: state(for: stage, cur))
                                .frame(maxWidth: .infinity)
                        }
                    }
                }
            }
            .frame(height: 14)
            HStack(spacing: 0) {
                ForEach(stages) { stage in
                    let stageState = state(for: stage, cur)
                    Text(stage.label)
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(stageState.active ? Theme.gold
                                          : (stageState.filled ? Color.primary : Color.secondary))
                        .frame(maxWidth: .infinity)
                }
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
