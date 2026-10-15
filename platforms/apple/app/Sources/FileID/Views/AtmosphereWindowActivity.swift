import AppKit
import SwiftUI

struct AtmosphereWindowActivity: NSViewRepresentable {
    let delivery: AtmosphereActivityDelivery
    var onChange: @MainActor (Bool) -> Void

    func makeNSView(context: Context) -> AtmosphereActivityView {
        AtmosphereActivityView(delivery: delivery, onChange: onChange)
    }

    func updateNSView(_ view: AtmosphereActivityView, context: Context) {
        view.onChange = onChange
        view.bindWindow()
    }

    static func dismantleNSView(_ view: AtmosphereActivityView, coordinator: ()) {
        view.dismantle()
    }
}

@MainActor
final class AtmosphereActivityView: NSView {
    var onChange: @MainActor (Bool) -> Void
    private let delivery: AtmosphereActivityDelivery
    private weak var observedWindow: NSWindow?
    private var bindingGeneration: UInt64?
    private var dismantled = false

    init(delivery: AtmosphereActivityDelivery, onChange: @escaping @MainActor (Bool) -> Void) {
        self.delivery = delivery
        self.onChange = onChange
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { nil }

    override func viewWillMove(toWindow newWindow: NSWindow?) {
        unbindWindow()
        super.viewWillMove(toWindow: newWindow)
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        bindWindow()
    }

    override func viewDidHide() {
        super.viewDidHide()
        refreshActivity()
    }

    override func viewDidUnhide() {
        super.viewDidUnhide()
        refreshActivity()
    }

    func bindWindow() {
        guard !dismantled else { return }
        guard observedWindow !== window else {
            refreshActivity()
            return
        }
        unbindWindow()
        guard let window else { return }
        observedWindow = window
        bindingGeneration = delivery.bind(to: ObjectIdentifier(window), at: ProcessInfo.processInfo.systemUptime) { [weak self] publication in
            guard let self, !self.dismantled,
                  self.bindingGeneration == publication.generation else { return }
            if publication.isDetached {
                // A closing window can remain attached until its native content is released.
                guard !publication.isActive, self.observedWindow == nil,
                      self.window == nil || self.window.map({ ObjectIdentifier($0) }) == publication.windowIdentity else { return }
            } else {
                guard let observedWindow = self.observedWindow,
                      self.window === observedWindow,
                      ObjectIdentifier(observedWindow) == publication.windowIdentity else { return }
            }
            self.onChange(publication.isActive)
        }
        let center = NotificationCenter.default
        for name in [NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification,
                     NSWindow.didMiniaturizeNotification, NSWindow.didDeminiaturizeNotification,
                     NSWindow.didChangeOcclusionStateNotification] {
            center.addObserver(self, selector: #selector(activityChanged(_:)), name: name, object: window)
        }
        center.addObserver(self, selector: #selector(windowClosing(_:)),
                           name: NSWindow.willCloseNotification, object: window)
        for name in [NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification,
                     NSApplication.didHideNotification, NSApplication.didUnhideNotification] {
            center.addObserver(self, selector: #selector(activityChanged(_:)), name: name, object: NSApp)
        }
        refreshActivity()
    }

    func unbindWindow(terminal: Bool = false) {
        NotificationCenter.default.removeObserver(self)
        observedWindow = nil
        guard let bindingGeneration else { return }
        let time = ProcessInfo.processInfo.systemUptime
        if terminal {
            delivery.invalidate(generation: bindingGeneration, at: time)
            self.bindingGeneration = nil
        } else if let generation = delivery.detach(generation: bindingGeneration, at: time) {
            self.bindingGeneration = generation
        }
    }

    func dismantle() {
        dismantled = true
        unbindWindow(terminal: true)
    }

    @objc private func activityChanged(_ notification: Notification) {
        refreshActivity()
    }

    @objc private func windowClosing(_ notification: Notification) {
        unbindWindow()
    }

    private func refreshActivity() {
        guard let window = observedWindow else { return }
        reportActivity(NSApp.isActive && !NSApp.isHidden && window.isKeyWindow
                       && window.isVisible && !window.isMiniaturized
                       && window.occlusionState.contains(.visible)
                       && !isHiddenOrHasHiddenAncestor)
    }

    private func reportActivity(_ active: Bool) {
        guard let window = observedWindow, let bindingGeneration else { return }
        delivery.record(active, for: ObjectIdentifier(window), generation: bindingGeneration,
                        at: ProcessInfo.processInfo.systemUptime)
    }
}
