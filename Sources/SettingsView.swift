import SwiftUI

struct SettingsView: View {
    @ObservedObject var viewModel: AppViewModel
    @AppStorage("classificationConfidence") private var confidence: Double = 0.30
    @AppStorage("faceClusterThreshold") private var faceThreshold: Double = 0.65
    @AppStorage("batchSize") private var batchSize: Int = 25
    
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                // Header
                HStack {
                    Image(systemName: "gearshape.2.fill")
                        .font(.title)
                        .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                    Text("Settings")
                        .font(.title.bold())
                    Spacer()
                }
                
                // Performance
                SettingsSection(title: "Performance", icon: "bolt.fill") {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("Processing Profile")
                            .font(.caption.bold())
                            .foregroundStyle(.secondary)
                        
                        Picker("Profile", selection: $viewModel.performanceProfile) {
                            Label("Low Power", systemImage: "leaf.fill").tag(0)
                            Label("Balanced", systemImage: "gauge.with.dots.needle.33percent").tag(1)
                            Label("Max Turbo", systemImage: "flame.fill").tag(2)
                        }
                        .pickerStyle(.segmented)
                        
                        Text(profileDescription)
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                    }
                    
                    Divider()
                    
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("Batch Save Interval")
                                .font(.caption.bold())
                                .foregroundStyle(.secondary)
                            Spacer()
                            Text("\(batchSize) files")
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                        }
                        Slider(value: Binding(get: { Double(batchSize) }, set: { batchSize = Int($0) }), in: 10...100, step: 5)
                            .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                        Text("Lower values = more responsive UI, higher values = faster processing")
                            .font(.system(size: 9))
                            .foregroundStyle(.tertiary)
                    }
                }
                
                // AI Configuration
                SettingsSection(title: "AI Classification", icon: "brain") {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("Classification Confidence")
                                .font(.caption.bold())
                                .foregroundStyle(.secondary)
                            Spacer()
                            Text(String(format: "%.0f%%", confidence * 100))
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                        }
                        Slider(value: $confidence, in: 0.1...0.9, step: 0.05)
                            .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                        Text("Lower = more tags (may include false positives). Higher = fewer, more accurate tags.")
                            .font(.system(size: 9))
                            .foregroundStyle(.tertiary)
                    }
                    
                    Divider()
                    
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text("Face Clustering Sensitivity")
                                .font(.caption.bold())
                                .foregroundStyle(.secondary)
                            Spacer()
                            Text(String(format: "%.2f", faceThreshold))
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                        }
                        Slider(value: $faceThreshold, in: 0.3...1.5, step: 0.05)
                            .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
                        Text("Lower = more aggressive merging (fewer identities). Higher = conservative (more identities, some may be duplicates).")
                            .font(.system(size: 9))
                            .foregroundStyle(.tertiary)
                    }
                }
                
                // Export
                SettingsSection(title: "Data", icon: "square.and.arrow.up") {
                    HStack {
                        Button {
                            exportLogs()
                        } label: {
                            Label("Export Logs to File", systemImage: "doc.text.fill")
                        }
                        .buttonStyle(.bordered)
                        
                        Spacer()
                        
                        Text("\(viewModel.logs.count) log entries")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                
                // System Info
                SettingsSection(title: "System", icon: "cpu") {
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("CPU Cores: \(ProcessInfo.processInfo.activeProcessorCount)")
                            Text("RAM: \(ProcessInfo.processInfo.physicalMemory / (1024 * 1024 * 1024)) GB")
                            Text("macOS \(ProcessInfo.processInfo.operatingSystemVersionString)")
                        }
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                        Spacer()
                    }
                }
            }
            .padding(30)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.3))
    }
    
    var profileDescription: String {
        switch viewModel.performanceProfile {
        case 0: return "Uses minimal CPU/GPU. Best for battery life on laptops."
        case 2: return "Maximum parallelism. Uses all cores + ANE. May cause fan noise."
        default: return "Balanced performance and power usage. Recommended for most users."
        }
    }
    
    func exportLogs() {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "FileID_Logs_\(Date().formatted(date: .numeric, time: .omitted)).txt"
        panel.allowedContentTypes = [.plainText]
        if panel.runModal() == .OK, let url = panel.url {
            let content = viewModel.logs.joined(separator: "\n")
            try? content.write(to: url, atomically: true, encoding: .utf8)
        }
    }
}

struct SettingsSection<Content: View>: View {
    let title: String
    let icon: String
    @ViewBuilder let content: Content
    
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 8) {
                Image(systemName: icon)
                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                Text(title)
                    .font(.headline)
            }
            
            VStack(alignment: .leading, spacing: 12) {
                content
            }
            .padding(16)
            .background(RoundedRectangle(cornerRadius: 12).fill(.ultraThinMaterial))
            .overlay(RoundedRectangle(cornerRadius: 12).stroke(Color.white.opacity(0.08), lineWidth: 1))
        }
    }
}
