import SwiftUI

struct EngineStatusRow: View {
    let state: EngineClient.ConnectionState
    var body: some View {
        switch state {
        case .starting:
            Label("Starting…", systemImage: "hourglass")
                .font(.callout)
                .foregroundStyle(.secondary)
        case .ready(let info):
            Label("Ready", systemImage: "checkmark.circle.fill")
                .font(.callout.bold())
                .foregroundStyle(.green)
                .help("\(info.workerCap) workers · \(Int(info.physicalMemoryGB)) GB RAM · pid \(info.pid)")
        case .crashed(let reason):
            Label(reason, systemImage: "xmark.octagon.fill")
                .font(.caption)
                .foregroundStyle(.red)
                .lineLimit(2)
        }
    }
}
