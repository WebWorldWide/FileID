import SwiftUI

// MARK: - AIModelSetupView

// Reusable model manager, embedded in Onboarding and Settings. Starting a
// download is the user's license acceptance — links to each license are inline.

struct AIModelSetupView: View {
    @Bindable var downloader: AIModelDownloadService = AIModelDownloadService.shared
    var showsHeader: Bool = true
    var compact: Bool = false

    private let goldColor = Theme.gold

    var body: some View {
        VStack(alignment: .leading, spacing: compact ? 10 : 16) {
            if showsHeader {
                header
            }
            ForEach(AIModelKind.allCases) { kind in
                ModelCard(
                    kind: kind,
                    status: downloader.status[kind] ?? .notInstalled,
                    onDownload: { downloader.download(kind) },
                    onCancel:   { downloader.cancel(kind) },
                    onDelete:   { downloader.delete(kind) },
                    compact: compact
                )
            }
            if !compact {
                Text("Models download from official Hugging Face repositories. Your files and this download stay on your Mac — nothing about your library is transmitted.")
                    .font(.system(size: 10))
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .onAppear { downloader.refreshStatus() }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "brain.head.profile.fill")
                .font(.system(size: 28))
                .foregroundStyle(goldColor)
            VStack(alignment: .leading, spacing: 2) {
                Text("AI Models (optional)")
                    .font(.system(size: 18, weight: .bold))
                Text("Adds semantic tagging, similarity search, and deep captions. All local, all private.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - ModelCard

private struct ModelCard: View {
    let kind: AIModelKind
    let status: AIModelDownloadService.ModelStatus
    let onDownload: () -> Void
    let onCancel:   () -> Void
    let onDelete:   () -> Void
    let compact: Bool

    private let goldColor = Theme.gold

    var descriptor: AIModelDescriptor { kind.descriptor }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: iconFor(kind))
                    .font(.system(size: 20))
                    .foregroundStyle(goldColor)
                    .frame(width: 28, alignment: .center)

                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(descriptor.displayName)
                            .font(.system(size: 13, weight: .semibold))
                        if descriptor.displayName.contains("Qwen") {
                            Image(systemName: "info.circle")
                                .font(.system(size: 10))
                                .foregroundStyle(.secondary)
                                .help("""
Qwen2.5-VL is Alibaba's open-weight VLM (Apache 2.0). FileID runs it 100% locally via Apple's MLX framework — no network calls, weights live on disk. We chose Qwen2.5-VL 3B because it outperforms same-size alternatives (LLaVA 1.6, Moondream, Phi-3.5-Vision) on DocVQA, ChartQA, and OCRBench. Since inference is fully offline, model country-of-origin does not affect data privacy.
""")
                        }
                    }
                    Text(descriptor.subtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if !compact {
                        Text(descriptor.reason)
                            .font(.system(size: 11))
                            .foregroundStyle(.tertiary)
                            .lineLimit(3)
                            .padding(.top, 2)
                    }
                }

                Spacer(minLength: 8)

                actionButton
            }

            if case .downloading(let p) = status {
                progressRow(progress: p)
            } else if case .failed(let msg) = status {
                Text("Error: \(msg)")
                    .font(.caption)
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }

            if !compact {
                HStack(spacing: 6) {
                    Text(descriptor.approxSizeString)
                        .font(.system(size: 10, design: .monospaced))
                        .padding(.horizontal, 5).padding(.vertical, 2)
                        .background(Capsule().fill(Color.white.opacity(0.08)))
                    Link(descriptor.licenseName, destination: descriptor.licenseURL)
                        .font(.system(size: 10))
                        .foregroundStyle(.tertiary)
                    if let repoURL = URL(string: "https://huggingface.co/\(descriptor.sourceRepo)") {
                        Link("Source", destination: repoURL)
                            .font(.system(size: 10))
                            .foregroundStyle(.tertiary)
                    }
                    Spacer()
                    Text(descriptor.attribution)
                        .font(.system(size: 9))
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(.ultraThinMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(statusColor.opacity(0.35), lineWidth: 1)
                )
        )
    }

    @ViewBuilder
    private var actionButton: some View {
        switch status {
        case .notInstalled:
            Button {
                onDownload()
            } label: {
                Label("Download", systemImage: "arrow.down.circle.fill")
                    .font(.caption.bold())
            }
            .buttonStyle(.borderedProminent)
            .tint(goldColor)
            .controlSize(.small)
            .help("Download this model (\(descriptor.approxSizeString))")
        case .queued:
            HStack(spacing: 6) {
                Label("Queued", systemImage: "clock")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                Button {
                    onCancel()
                } label: {
                    Image(systemName: "xmark.circle")
                        .font(.caption)
                }
                .buttonStyle(.borderless)
                .help("Remove from queue")
            }
        case .downloading:
            Button {
                onCancel()
            } label: {
                Label("Cancel", systemImage: "xmark.circle")
                    .font(.caption)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .help("Cancel download and remove partial files")
        case .installed:
            HStack(spacing: 6) {
                Label("Installed", systemImage: "checkmark.circle.fill")
                    .font(.caption.bold())
                    .foregroundStyle(.green)
                Button {
                    onDelete()
                } label: {
                    Image(systemName: "trash")
                        .font(.caption)
                }
                .buttonStyle(.borderless)
                .help("Delete this model (~\(descriptor.approxSizeString))")
            }
        case .failed:
            Button {
                onDownload()
            } label: {
                Label("Retry", systemImage: "arrow.clockwise")
                    .font(.caption)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .help("Retry the failed download")
        }
    }

    private func progressRow(progress p: AIModelDownloadService.Progress) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            ProgressView(value: p.fractionComplete)
                .progressViewStyle(.linear)
                .tint(goldColor)
            HStack(spacing: 6) {
                Text("\(p.fileIndex)/\(p.fileTotal)  \(p.currentFile)")
                    .font(.system(size: 10, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer()
                Text("\(formatBytes(p.bytesDownloaded)) / \(formatBytes(p.bytesExpected))")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var statusColor: Color {
        switch status {
        case .installed:    return .green
        case .downloading:  return goldColor
        case .queued:       return .blue
        case .failed:       return .red
        case .notInstalled: return .white
        }
    }

    private func iconFor(_ kind: AIModelKind) -> String {
        switch kind {
        case .mobileCLIPImage: return "photo.stack"
        case .mobileCLIPText:  return "text.magnifyingglass"
        case .qwen2VL2B:       return "sparkles.rectangle.stack"
        case .qwen3VL4B:       return "sparkles.rectangle.stack.fill"
        case .gemma3_4B:       return "g.circle"
        case .gemma3_12B:      return "g.circle.fill"
        case .smolvlm:         return "bolt.circle"
        case .paligemma3B:     return "rectangle.stack.badge.person.crop"
        }
    }

    private func formatBytes(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }
}
