import SwiftUI

enum RestructureStepState { case done, current, upcoming }

/// One of the three Restructure steps (Preview · Review · Move). The current
/// step gets the gold selection wash and rail; finished steps show a pink
/// check; upcoming steps are dimmed.
struct RestructureStepCell<Actions: View>: View {
    let number: Int
    let title: String
    let state: RestructureStepState
    let detail: String
    @ViewBuilder var actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                badge
                Text(title).font(Theme.Font.body(13.5, .semibold))
                    .foregroundStyle(state == .upcoming ? Theme.text3 : Theme.text1)
                Spacer(minLength: 0)
            }
            Text(detail).font(Theme.Font.body(12)).foregroundStyle(Theme.text3)
                .lineLimit(2).fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
            HStack(spacing: 8) { actions }
                .frame(minHeight: Theme.Metrics.hitTarget, alignment: .leading)
        }
        .padding(.horizontal, 14).padding(.vertical, 12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(state == .current ? Theme.goldWash : Color.clear, ignoresSafeAreaEdges: [])
        .overlay(alignment: .leading) { if state == .current { Rectangle().fill(Theme.gold).frame(width: 3) } }
        .animation(Theme.springStandard, value: state)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Step \(number) of 3, \(title), \(stateLabel)")
        .accessibilityHint(detail)
    }

    private var stateLabel: String {
        switch state {
        case .done: return "done"
        case .current: return "current"
        case .upcoming: return "not started"
        }
    }

    @ViewBuilder
    private var badge: some View {
        ZStack {
            switch state {
            case .done:
                Circle().fill(Theme.delight)
                Image(systemName: "checkmark").font(.system(size: 9, weight: .heavy)).foregroundStyle(Theme.onGold)
            case .current:
                Circle().stroke(Theme.gold, lineWidth: 1.5)
                Text("\(number)").font(Theme.Font.mono(11, .semibold)).foregroundStyle(Theme.gold)
            case .upcoming:
                Circle().stroke(Theme.hairlineStrong, lineWidth: 1.5)
                Text("\(number)").font(Theme.Font.mono(11, .semibold)).foregroundStyle(Theme.text3)
            }
        }
        .frame(width: 20, height: 20)
        .accessibilityHidden(true)
    }
}
