// Pages stay mounted after their first visit so a tab switch keeps scroll
// position, loaded rows and selections. Hidden pages read `\.pageActive`
// and hold their reloads until they are shown again.
import SwiftUI
import AppKit

/// Which pages stay mounted: the most recently used first, capped, with
/// transient pages dropped as soon as they are left.
struct KeptPages<Page: Hashable>: Equatable {
    let limit: Int
    let transient: Set<Page>
    private(set) var recent: [Page] = []

    init(limit: Int, transient: Set<Page> = []) {
        self.limit = max(1, limit)
        self.transient = transient
    }

    var active: Page? { recent.first }

    func contains(_ page: Page) -> Bool { recent.contains(page) }

    mutating func visit(_ page: Page) {
        guard recent.first != page else { return }
        recent.removeAll { $0 == page || transient.contains($0) }
        recent.insert(page, at: 0)
        if recent.count > limit { recent.removeLast(recent.count - limit) }
    }

    func visiting(_ page: Page) -> KeptPages {
        var next = self
        next.visit(page)
        return next
    }
}

private struct PageActiveKey: EnvironmentKey {
    static let defaultValue = true
}

extension EnvironmentValues {
    /// False while a kept-alive page is mounted but not the one on screen.
    /// True everywhere else (sheets, the side Chat panel, the tools window).
    var pageActive: Bool {
        get { self[PageActiveKey.self] }
        set { self[PageActiveKey.self] = newValue }
    }
}

/// Runs `action` when `value` changes on a visible page. A change that lands
/// while the page is hidden runs it once when the page is shown again.
private struct ChangeWhileActive<Value: Equatable>: ViewModifier {
    let value: Value
    let action: () -> Void
    @Environment(\.pageActive) private var active
    @State private var missed = false

    func body(content: Content) -> some View {
        content
            .onChange(of: value) { _, _ in
                if active { action() } else { missed = true }
            }
            .onChange(of: active) { _, now in
                guard now, missed else { return }
                missed = false
                action()
            }
    }
}

private struct PageVisibility: ViewModifier {
    let shown: () -> Void
    let hidden: () -> Void
    @Environment(\.pageActive) private var active

    func body(content: Content) -> some View {
        content.onChange(of: active) { _, now in
            if now { shown() } else { hidden() }
        }
    }
}

extension View {
    func onChangeWhileActive<Value: Equatable>(of value: Value, perform action: @escaping () -> Void) -> some View {
        modifier(ChangeWhileActive(value: value, action: action))
    }

    /// `shown` runs when a kept-alive page returns to the screen (not on its
    /// first mount, which is `onAppear`); `hidden` when another page covers it.
    func onPageVisibility(shown: @escaping () -> Void = {}, hidden: @escaping () -> Void = {}) -> some View {
        modifier(PageVisibility(shown: shown, hidden: hidden))
    }
}

/// Gives up keyboard focus held inside the page area. A destroyed page used to
/// lose it with its views; a hidden one would keep taking keystrokes.
@MainActor
final class PageAreaFocus {
    weak var view: NSView?

    func release() {
        guard let view, let window = view.window, let responder = window.firstResponder as? NSView,
              responder !== window.contentView else { return }
        let area = view.convert(view.bounds, to: nil)
        let frame = responder.convert(responder.bounds, to: nil)
        if area.contains(CGPoint(x: frame.midX, y: frame.midY)) { window.makeFirstResponder(nil) }
    }
}

struct PageAreaProbe: NSViewRepresentable {
    let focus: PageAreaFocus

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        focus.view = view
        return view
    }

    func updateNSView(_ view: NSView, context: Context) { focus.view = view }
}
