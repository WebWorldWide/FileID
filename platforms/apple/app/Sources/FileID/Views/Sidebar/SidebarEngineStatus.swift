import SwiftUI

struct EngineStatusRow: View {
    let state: EngineClient.ConnectionState
    var body: some View {
        switch state {
        case .starting:
            Label("Getting ready…", systemImage: "hourglass")
                .font(Theme.Font.body(13))
                .foregroundStyle(Theme.text3)
                .help("FileID is starting its background helper. This takes a few seconds.")
        case .ready(let info):
            Label("Ready", systemImage: "checkmark.circle.fill")
                .font(Theme.Font.body(13.5, .semibold))
                .foregroundStyle(Theme.delight)
                .help("\(info.workerCap) workers · \(Int(info.physicalMemoryGB)) GB RAM · pid \(info.pid)")
        case .crashed(let reason):
            Label(reason, systemImage: "xmark.octagon.fill")
                .help("FileID's background helper stopped. It restarts on its own; if this stays, quit and reopen FileID.")
                .font(Theme.Font.body(12))
                .foregroundStyle(Theme.danger)
                .lineLimit(2)
        }
    }
}
