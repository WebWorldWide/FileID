import SwiftUI

struct EngineStatusRow: View {
    let state: EngineClient.ConnectionState
    var onRestart: (() -> Void)? = nil
    var body: some View {
        switch state {
        case .starting:
            Label("Getting ready…", systemImage: "hourglass")
                .font(Theme.ChromeFont.body(13))
                .foregroundStyle(Theme.text3)
                .help("FileID is starting its background helper. This takes a few seconds.")
        case .ready(let info):
            Label("Ready", systemImage: "checkmark.circle.fill")
                .font(Theme.ChromeFont.body(13.5, .semibold))
                .foregroundStyle(Theme.delight)
                .help("\(info.workerCap) workers · \(Int(info.physicalMemoryGB)) GB RAM · pid \(info.pid)")
        case .crashed(let reason):
            VStack(alignment: .leading, spacing: 6) {
                Label("FileID's helper stopped", systemImage: "xmark.octagon.fill")
                    .font(Theme.ChromeFont.body(12.5, .semibold))
                    .foregroundStyle(Theme.danger)
                    .help(reason)
                if let onRestart {
                    Button("Restart") { onRestart() }
                        .buttonStyle(DSSecondaryButtonStyle(height: 24))
                        .help("Start the helper again. If it keeps stopping, quit and reopen FileID.")
                }
            }
        }
    }
}
