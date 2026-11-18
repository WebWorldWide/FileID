// Shown over the pages when FileID's engine (Contents/MacOS/fileidd) can't start or keeps
// stopping. There is no second engine to fall back to, so the card says what happened and
// offers the two things a person can do: look at the log, and try again.
import SwiftUI
import AppKit

struct EngineUnavailableCard: View {
    let failure: EngineStartFailure
    var onTryAgain: @MainActor () -> Void
    var onShowLog: @MainActor () -> Void = EngineUnavailableCard.showLog

    var body: some View {
        Panel(padding: 28) {
            VStack(spacing: 14) {
                Image(systemName: "exclamationmark.triangle")
                    .font(.system(size: 44, weight: .light))
                    .foregroundStyle(Theme.gold)
                    .accessibilityHidden(true)
                Text(Self.title)
                    .font(Theme.Font.body(20, .semibold))
                    .multilineTextAlignment(.center)
                    .accessibilityAddTraits(.isHeader)
                Text(failure.summary)
                    .font(Theme.Font.body(13.5))
                    .foregroundStyle(Theme.text2)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                Text(Self.reinstallHint(for: failure))
                    .font(Theme.Font.body(12.5))
                    .foregroundStyle(Theme.text3)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 10) {
                    Button("Show log", action: onShowLog)
                        .buttonStyle(DSSecondaryButtonStyle(height: 32))
                        .help("Open FileID's log to see why the engine stopped.")
                    Button("Try again", action: onTryAgain)
                        .buttonStyle(DSPrimaryButtonStyle(height: 32))
                        .keyboardShortcut(.defaultAction)
                        .help("Start FileID's engine again.")
                }
                .padding(.top, 6)
            }
            .frame(maxWidth: .infinity)
        }
        .frame(maxWidth: 460)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(Self.title). \(failure.summary)")
    }

    static let title = "FileID's engine couldn't start"

    static func reinstallHint(for failure: EngineStartFailure) -> String {
        failure.suggestsReinstall
            ? "Reinstall FileID from the original download to put it back. Your library and settings stay where they are."
            : "If it keeps happening, reinstall FileID from the original download. Your library and settings stay where they are."
    }

    /// The log is app.log; before it exists, show the folder FileID keeps its files in.
    static func showLog() {
        let log = SettingsTab.appLogURL
        if FileManager.default.fileExists(atPath: log.path) {
            NSWorkspace.shared.open(log)
        } else {
            NSWorkspace.shared.activateFileViewerSelecting([AppSupportPath.fileID])
        }
    }
}

/// The card, centred over a page, while the engine can't run.
struct EngineUnavailableOverlay: ViewModifier {
    let engine: EngineClient
    let activeTab: MainWindow.Tab

    func body(content: Content) -> some View {
        content.overlay {
            if let failure = engine.startFailure, activeTab != .settings {
                ZStack {
                    Rectangle().fill(Color(red: 15 / 255, green: 15 / 255, blue: 17 / 255))
                    EngineUnavailableCard(failure: failure, onTryAgain: { engine.restart() })
                        .padding(24)
                }
                .transition(.opacity)
            }
        }
    }
}

extension View {
    func engineUnavailableOverlay(engine: EngineClient, activeTab: MainWindow.Tab) -> some View {
        modifier(EngineUnavailableOverlay(engine: engine, activeTab: activeTab))
    }
}
