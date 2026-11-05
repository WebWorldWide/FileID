import SwiftUI
import Observation

/// Settings ▸ Appearance ▸ Text size. Scales page text (file names, details,
/// descriptions, chat); the title bar and sidebar keep their designed size.
enum TextSize: String, CaseIterable, Identifiable, Sendable {
    case small, standard, large, larger

    static let storageKey = "appearance.textSize"

    var id: String { rawValue }

    init(stored: String?) { self = stored.flatMap(TextSize.init(rawValue:)) ?? .standard }

    var title: String {
        switch self {
        case .small: return "Small"
        case .standard: return "Default"
        case .large: return "Large"
        case .larger: return "Larger"
        }
    }

    var factor: CGFloat {
        switch self {
        case .small: return 0.9
        case .standard: return 1
        case .large: return 1.15
        case .larger: return 1.3
        }
    }

    /// Half-point steps keep glyphs on the same grid the designed sizes use;
    /// Default returns the size untouched.
    func scaled(_ size: CGFloat) -> CGFloat {
        self == .standard ? size : (size * factor * 2).rounded() / 2
    }
}

/// The live text size, read by `Theme.Font` while views compute their body,
/// so every view that draws scaled text re-renders when it changes. Fonts are
/// also resolved from Canvas renderers, hence the lock instead of main-actor
/// isolation.
final class ContentTextScale: Observable, @unchecked Sendable {
    static let shared = ContentTextScale(size: TextSize(stored: UserDefaults.standard.string(forKey: TextSize.storageKey)))

    private let registrar = ObservationRegistrar()
    private let lock = NSLock()
    private var stored: TextSize

    init(size: TextSize) { stored = size }

    var size: TextSize {
        get {
            registrar.access(self, keyPath: \.size)
            return lock.withLock { stored }
        }
        set {
            guard newValue != lock.withLock({ stored }) else { return }
            registrar.withMutation(of: self, keyPath: \.size) { lock.withLock { stored = newValue } }
        }
    }
}

private struct FixedTextSizeKey: EnvironmentKey { static let defaultValue = false }

extension EnvironmentValues {
    /// True under the title bar and sidebar: shared controls there keep the designed size.
    var fixedTextSize: Bool {
        get { self[FixedTextSizeKey.self] }
        set { self[FixedTextSizeKey.self] = newValue }
    }
}

extension View {
    /// Window chrome: text stays at the designed size whatever Text size is set to.
    func chromeTextSize() -> some View {
        environment(\.fixedTextSize, true).font(Theme.ChromeFont.body(13.5))
    }
}
