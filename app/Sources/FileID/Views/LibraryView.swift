// Library: DB-backed thumbnail grid. FTS5 search (OCR + filename) +
// kind filter; re-queries on each batchSummary event for live fill-in.
import SwiftUI
import AppKit
import PDFKit
import FileIDShared

struct LibraryView: View {
    let engine: EngineClient
    let store: ReadStore

    @State private var rows: [FileRow] = []
    @State private var searchText: String = ""
    @State private var kindFilter: String? = nil
    @State private var lastSeenVersion: Int = -1
    @State private var lastSeenBatchIndex: Int = -1
    @State private var lastReloadAt: Date = .distantPast
    @State private var selected: FileRow?

    private let columns = [
        GridItem(.adaptive(minimum: 160, maximum: 220), spacing: 12)
    ]

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().opacity(0.4)
            if let p = engine.lastProgress,
               p.phase == .discovering || p.phase == .tagging || p.phase == .postScan {
                inFlightHeadline(p)
            }
            if engine.deepAnalyzeInFlight {
                deepAnalyzeHeadline()
            }
            if rows.isEmpty {
                empty
            } else {
                grid
            }
        }
        .onAppear {
            store.openIfPossible()
            reload()
        }
        .onChange(of: engine.lastBatch?.batchIndex ?? -1) { _, new in
            if new != lastSeenBatchIndex {
                lastSeenBatchIndex = new
                guard Date().timeIntervalSince(lastReloadAt) >= 1.0 else { return }
                lastReloadAt = Date()
                store.notifyChanged()
                reload()
            }
        }
        .onChange(of: searchText) { _, _ in reload() }
        .onChange(of: kindFilter) { _, _ in reload() }
    }

    // MARK: - Live progress headline

    @ViewBuilder
    private func inFlightHeadline(_ p: ScanProgress) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                Image(systemName: "brain.head.profile")
                    .foregroundStyle(Theme.gold)
                    .font(.title3)
                VStack(alignment: .leading, spacing: 1) {
                    Text(p.phase == .discovering ? "Discovering files…"
                         : p.phase == .tagging ? "Tagging files…" : "Post-scan…")
                        .font(.headline)
                    Text("Tags + face detection. Smart names come later via Deep Analyze.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Text("\(p.processed) / \(p.total)  ·  \(p.discovered) found")
                    .font(.callout.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            if p.total > 0 {
                ProgressView(value: Double(p.processed),
                             total: Double(max(p.total, 1)))
                    .tint(Theme.gold)
            } else if p.phase == .discovering {
                ProgressView()
                    .progressViewStyle(.linear)
                    .tint(Theme.gold)
            }
            HStack(spacing: 16) {
                Text(String(format: "%.1f files/s", p.filesPerSecond))
                    .foregroundStyle(Theme.gold)
                if let eta = p.etaSeconds, eta > 0 {
                    Text("ETA \(formatETA(eta))")
                }
                Spacer()
                Text("\(p.residentMB) MB resident")
                    .foregroundStyle(p.residentMB > 1200 ? .orange : .secondary)
                Text("\(p.availableMB) MB free")
                    .foregroundStyle(.secondary)
                if p.failed > 0 {
                    Text("\(p.failed) failed").foregroundStyle(.red)
                }
            }
            .font(.caption.monospacedDigit())
        }
        .padding(16)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(.ultraThinMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(Theme.gold.opacity(0.4), lineWidth: 1)
                )
        )
        .padding(.horizontal, 20)
        .padding(.top, 16)
    }

    private func formatETA(_ seconds: Double) -> String {
        let s = Int(seconds.rounded())
        let h = s / 3600; let m = (s % 3600) / 60; let sec = s % 60
        if h > 0 { return String(format: "%dh %dm", h, m) }
        if m > 0 { return String(format: "%dm %ds", m, sec) }
        return "\(sec)s"
    }

    // MARK: - Deep Analyze live headline

    @ViewBuilder
    private func deepAnalyzeHeadline() -> some View {
        let p = engine.deepAnalyzeProgress
        let last = engine.deepAnalyzeLast
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                Image(systemName: "sparkles")
                    .foregroundStyle(Theme.gold)
                    .font(.title3)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Deep Analyze running…")
                        .font(.headline)
                    Text("Local VLM is captioning images and proposing smart filenames.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if let p {
                    Text("\(p.processed) / \(p.total)")
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }
            if let p, p.total > 0 {
                ProgressView(value: Double(p.processed),
                             total: Double(max(p.total, 1)))
                    .tint(Theme.gold)
            } else {
                ProgressView()
                    .progressViewStyle(.linear)
                    .tint(Theme.gold)
            }
            if let p {
                HStack(spacing: 16) {
                    if let eta = p.etaSeconds, eta > 0 {
                        Text("ETA \(formatETA(eta))")
                    }
                    if let cur = p.currentPath {
                        Text("Now: \((cur as NSString).lastPathComponent)")
                            .lineLimit(1).truncationMode(.middle)
                    }
                    Spacer()
                }
                .font(.caption.monospacedDigit())
                .foregroundStyle(.tertiary)
            }
            if let last {
                Divider().opacity(0.3)
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: "wand.and.rays")
                        .foregroundStyle(Theme.gold)
                        .font(.callout)
                    VStack(alignment: .leading, spacing: 4) {
                        if let n = last.proposedName {
                            Text(n)
                                .font(.callout.monospaced().bold())
                                .foregroundStyle(Theme.gold)
                        }
                        Text(last.description)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                    }
                    Spacer()
                }
            }
        }
        .padding(16)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(.ultraThinMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(Theme.gold.opacity(0.4), lineWidth: 1)
                )
        )
        .padding(.horizontal, 20)
        .padding(.top, 16)
    }

    // MARK: - Header

    @ViewBuilder
    private var header: some View {
        HStack(alignment: .center, spacing: 12) {
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search filenames + OCR text…", text: $searchText)
                    .textFieldStyle(.plain)
                    .frame(minWidth: 220)
                if !searchText.isEmpty {
                    Button { searchText = "" } label: {
                        Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                    }
                    .buttonStyle(.plain)
                    .help("Clear search")
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: 360)
            .background(.ultraThinMaterial)
            .clipShape(Capsule())

            Spacer(minLength: 8)

            kindPicker

            Text("\(rows.count) of \(store.totalFiles)")
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .frame(minWidth: 80, alignment: .trailing)
        }
        .padding(20)
    }

    @ViewBuilder
    private var kindPicker: some View {
        let kinds: [(label: String, value: String?)] = [
            ("All",      nil),
            ("Images",   "image"),
            ("Videos",   "video"),
            ("Docs",     "doc"),
            ("PDFs",     "pdf"),
            ("Audio",    "audio")
        ]
        HStack(spacing: 2) {
            ForEach(Array(kinds.enumerated()), id: \.offset) { _, k in
                let active = kindFilter == k.value
                Button {
                    withAnimation(.easeInOut(duration: 0.15)) { kindFilter = k.value }
                } label: {
                    Text(k.label)
                        .font(.system(size: 11, weight: active ? .bold : .medium))
                        .foregroundStyle(active ? Color.black : Color.primary.opacity(0.7))
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(Capsule().fill(active ? Theme.gold : Color.white.opacity(0.06)))
                }
                .buttonStyle(.plain)
            }
        }
    }

    // MARK: - Grid

    @ViewBuilder
    private var grid: some View {
        ScrollView {
            LazyVGrid(columns: columns, spacing: 12) {
                ForEach(rows) { row in
                    FileTile(row: row, store: store)
                        .onTapGesture { selected = row }
                        .background(
                            RoundedRectangle(cornerRadius: Theme.Radius.m)
                                .stroke(selected?.id == row.id ? Theme.gold : Color.clear,
                                        lineWidth: 2)
                        )
                }
            }
            .padding(20)
        }
        .sheet(item: $selected) { file in
            FilePreviewSheet(file: file, store: store, engine: engine,
                              siblings: rows, onSelect: { selected = $0 })
        }
    }

    @ViewBuilder
    private var empty: some View {
        VStack(spacing: 16) {
            Image(systemName: "photo.on.rectangle.angled")
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(Theme.gold.opacity(0.5))
            if store.totalFiles == 0 {
                Text("No files in the library yet")
                    .font(.title3.bold())
                Text("Tagged files appear here in real time once a scan runs.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 400)
            } else {
                Text("No files match the current filter")
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Data

    private func reload() {
        rows = store.files(search: searchText, kindFilter: kindFilter)
    }
}

// MARK: - One tile

struct FileTile: View {
    let row: FileRow
    let store: ReadStore

    @State private var thumb: NSImage?
    @State private var hovering = false

    private var kindColor: Color {
        switch row.kind {
        case "image": return Theme.gold
        case "video": return .purple
        case "pdf":   return .red
        case "doc":   return .blue
        case "audio": return .pink
        default:      return .secondary
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // 1:1 carrier + overlay image: stable across portrait/landscape.
            Color.white.opacity(0.04)
                .aspectRatio(1, contentMode: .fit)
                .overlay(thumbContent)
                .clipShape(RoundedRectangle(cornerRadius: 10))
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(
                            hovering ? Theme.gold.opacity(0.65) : Color.white.opacity(0.10),
                            lineWidth: hovering ? 1.5 : 1
                        )
                )
                .scaleEffect(hovering ? 1.015 : 1.0)
                .shadow(color: .black.opacity(hovering ? 0.4 : 0.15),
                        radius: hovering ? 10 : 4, x: 0, y: 2)
                .animation(.easeInOut(duration: 0.12), value: hovering)
                .onHover { hovering = $0 }
                .overlay(badgeOverlay)

            // VLM-suggested name in gold (when present), real filename below.
            VStack(alignment: .leading, spacing: 2) {
                if let suggested = row.vlmProposedName, !suggested.isEmpty {
                    HStack(spacing: 3) {
                        Image(systemName: "sparkles")
                            .font(.system(size: 9))
                            .foregroundStyle(Theme.gold)
                        Text(suggested)
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(Theme.gold)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    .help("Smart name from Deep Analyze. Open the file to apply.")
                }
                Text(row.url.lastPathComponent)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(row.vlmProposedName == nil ? Color.primary : Color.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            HStack(spacing: 4) {
                Text(formatBytes(row.sizeBytes))
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
                Spacer()
                if let date = row.displayDate {
                    Text(date.formatted(date: .numeric, time: .omitted))
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .task {
            thumb = await ThumbnailService.shared.thumbnail(for: row.url, size: 264)
        }
    }

    @ViewBuilder
    private var thumbContent: some View {
        if let thumb {
            Image(nsImage: thumb)
                .resizable()
                .scaledToFill()
        } else {
            VStack(spacing: 4) {
                Image(systemName: kindIcon)
                    .font(.system(size: 36))
                    .foregroundStyle(kindColor.opacity(0.7))
                Text(row.extension.uppercased())
                    .font(.caption2.monospaced().bold())
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var badgeOverlay: some View {
        ZStack(alignment: .topLeading) {
            // Kind badge top-left.
            Text(row.kind.uppercased())
                .font(.system(size: 8, weight: .bold, design: .monospaced))
                .foregroundStyle(.black)
                .padding(.horizontal, 5)
                .padding(.vertical, 2)
                .background(Capsule().fill(kindColor.opacity(0.95)))
                .padding(6)

            // Faces / OCR-text indicators top-right.
            VStack(spacing: 4) {
                if row.hasFaces {
                    Image(systemName: "person.crop.circle.fill")
                        .font(.system(size: 14))
                        .foregroundStyle(.white, .black.opacity(0.6))
                        .help("Faces detected")
                }
                if row.hasText {
                    Image(systemName: "text.viewfinder")
                        .font(.system(size: 13))
                        .foregroundStyle(.white, .black.opacity(0.6))
                        .help("OCR text available")
                }
            }
            .padding(6)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
        }
        .allowsHitTesting(false)
    }

    private var kindIcon: String {
        switch row.kind {
        case "image": return "photo"
        case "video": return "video"
        case "pdf":   return "doc.richtext"
        case "doc":   return "doc.text"
        case "audio": return "music.note"
        default:      return "doc"
        }
    }

    private func formatBytes(_ b: Int64) -> String {
        let kb = Double(b) / 1024
        if kb < 1024 { return String(format: "%.0f KB", kb) }
        return String(format: "%.1f MB", kb / 1024)
    }
}

// MARK: - Preview sheet
//
// Full-bleed preview + metadata panel + tags. Reveal-in-Finder.
private struct FilePreviewSheet: View {
    let file: FileRow
    let store: ReadStore
    let engine: EngineClient
    let siblings: [FileRow]              // for prev/next arrow nav
    let onSelect: (FileRow) -> Void
    @Environment(\.dismiss) var dismiss
    @State private var preview: NSImage?

    private var siblingIndex: Int? {
        siblings.firstIndex(where: { $0.id == file.id })
    }

    private func step(_ delta: Int) {
        guard let idx = siblingIndex else { return }
        let target = idx + delta
        guard siblings.indices.contains(target) else { return }
        onSelect(siblings[target])
    }

    var body: some View {
        VStack(spacing: 0) {
            // Toolbar.
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(file.url.lastPathComponent)
                        .font(.title3.bold())
                        .lineLimit(1).truncationMode(.middle)
                    Text(file.url.deletingLastPathComponent().path)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1).truncationMode(.head)
                }
                Spacer()
                if let idx = siblingIndex {
                    Text("\(idx + 1) of \(siblings.count)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                    HStack(spacing: 4) {
                        Button { step(-1) } label: {
                            Image(systemName: "chevron.left")
                        }
                        .buttonStyle(.bordered)
                        .keyboardShortcut(.leftArrow, modifiers: [])
                        .disabled(idx == 0)
                        .help("Previous file")
                        Button { step(1) } label: {
                            Image(systemName: "chevron.right")
                        }
                        .buttonStyle(.bordered)
                        .keyboardShortcut(.rightArrow, modifiers: [])
                        .disabled(idx == siblings.count - 1)
                        .help("Next file")
                    }
                }
                if file.kind == "image" {
                    DeepAnalyzeButton(engine: engine, file: file)
                }
                Button {
                    NSWorkspace.shared.open(file.url)
                } label: {
                    Label("Open", systemImage: "arrow.up.right.square")
                        .font(.system(size: 12))
                }
                .buttonStyle(.bordered)
                .help("Open in the default app (Preview, QuickTime, etc.)")
                Button {
                    NSWorkspace.shared.activateFileViewerSelecting([file.url])
                } label: {
                    Label("Show in Finder", systemImage: "folder")
                        .font(.system(size: 12))
                }
                .buttonStyle(.bordered)
                .help("Reveal this file in Finder")
                Button {
                    dismiss()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 20))
                        .symbolRenderingMode(.palette)
                        .foregroundStyle(.white.opacity(0.7), .white.opacity(0.15))
                }
                .buttonStyle(.plain)
                .keyboardShortcut(.escape)
                .help("Close preview")
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
            .background(.ultraThinMaterial)

            HStack(spacing: 0) {
                // Preview canvas — kind-specific for fidelity, QLPreview as
                // the universal fallback for any other file type.
                Group {
                    switch file.kind {
                    case "video":
                        VideoPreview(url: file.url)
                    case "audio":
                        AudioPreview(url: file.url)
                    case "image":
                        ZStack {
                            Color.black.opacity(0.3)
                            if let preview {
                                Image(nsImage: preview)
                                    .resizable()
                                    .aspectRatio(contentMode: .fit)
                                    .padding(20)
                            } else {
                                VStack {
                                    ProgressView()
                                    Text("Loading preview…")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .padding(.top, 8)
                                }
                            }
                        }
                    case "pdf":
                        PDFPreview(url: file.url)
                    default:
                        // doc, archive, anything else — Quick Look thumbnail
                        // rendered as static NSImage. Toolbar's "Open with
                        // default app" hands off the live experience.
                        UniversalPreview(url: file.url)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)

                Divider().opacity(0.3)

                // Metadata panel.
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        GlassCard {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("Metadata").font(.headline)
                                Divider().opacity(0.3)
                                row("Path",   file.pathText)
                                row("Kind",   file.kind)
                                row("Size",   String(format: "%.2f MB", file.sizeMB))
                                if let d = file.displayDate {
                                    row("Date", d.formatted(date: .long, time: .shortened))
                                }
                                if let cm = file.cameraModel {
                                    row("Camera", cm)
                                }
                                if let lat = file.locationLat, let lon = file.locationLon {
                                    row("GPS", String(format: "%.5f, %.5f", lat, lon))
                                }
                                if file.hasFaces { row("Faces", "Detected") }
                                if file.hasText  { row("Text",  "Detected (OCR)") }
                                if let phash = file.phash {
                                    row("pHash", String(phash, radix: 16))
                                }
                                if let aest = file.aesthetic {
                                    row("Aesthetic", String(format: "%.2f", aest))
                                }
                            }
                        }
                        if let caption = file.vlmDescription, !caption.isEmpty {
                            GlassCard {
                                VStack(alignment: .leading, spacing: 8) {
                                    HStack {
                                        Text("Deep Analyze").font(.headline)
                                        Spacer()
                                        if let model = file.vlmModel,
                                           let kind = AIModelKind(rawValue: model) {
                                            BadgePill(label: kind.displayName, color: .secondary)
                                        }
                                    }
                                    Text(caption).font(.callout)
                                    if let proposed = file.vlmProposedName, !proposed.isEmpty {
                                        Divider().opacity(0.3)
                                        HStack(spacing: 8) {
                                            Image(systemName: "wand.and.rays")
                                                .foregroundStyle(Theme.gold)
                                            VStack(alignment: .leading, spacing: 2) {
                                                Text("Suggested name")
                                                    .font(.caption.bold())
                                                    .foregroundStyle(.secondary)
                                                Text("\(proposed).\(file.extension)")
                                                    .font(.caption.monospaced())
                                                    .foregroundStyle(Theme.gold)
                                            }
                                            Spacer()
                                            Button("Apply") {
                                                _ = store.applyProposedName(file: file)
                                                dismiss()
                                            }
                                            .buttonStyle(.bordered)
                                            .help("Renames the file on disk and updates the library row.")
                                        }
                                    }
                                }
                            }
                        }
                        let tags = store.tags(forFileID: file.id)
                        if !tags.isEmpty {
                            GlassCard {
                                VStack(alignment: .leading, spacing: 8) {
                                    Text("Tags").font(.headline)
                                    FlowLayout(spacing: 6) {
                                        ForEach(tags, id: \.self) { tag in
                                            BadgePill(label: tag)
                                        }
                                    }
                                }
                            }
                        }
                    }
                    .padding(16)
                }
                .frame(width: 360)
            }
        }
        .frame(minWidth: 960, minHeight: 600)
        .background(LavaLampBackground())
        .preferredColorScheme(.dark)
        .task {
            // Generate a larger preview for the sheet (640px).
            preview = await ThumbnailService.shared.thumbnail(for: file.url, size: 640)
        }
    }

    @ViewBuilder
    private func row(_ k: String, _ v: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(k).font(.caption.bold()).foregroundStyle(.secondary).frame(width: 70, alignment: .trailing)
            Text(v).font(.caption.monospaced()).textSelection(.enabled).lineLimit(3)
            Spacer()
        }
    }
}

/// Video preview: poster frame + Play button (hands off to the default
/// app). AVKit's NSViewRepresentable crashes on macOS 26 in SwiftUI's
/// eager sheet-branch metadata init.
private struct VideoPreview: View {
    let url: URL
    @State private var poster: NSImage?

    var body: some View {
        ZStack {
            Color.black
            if let poster {
                Image(nsImage: poster)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .padding(20)
            }
            VStack(spacing: 12) {
                Button {
                    NSWorkspace.shared.open(url)
                } label: {
                    ZStack {
                        Circle().fill(.black.opacity(0.55))
                            .frame(width: 96, height: 96)
                        Circle().stroke(Theme.gold, lineWidth: 3)
                            .frame(width: 96, height: 96)
                        Image(systemName: "play.fill")
                            .font(.system(size: 44))
                            .foregroundStyle(Theme.gold)
                            .offset(x: 4)
                    }
                }
                .buttonStyle(.plain)
                .help("Play in QuickTime / default video app")
                Text("Click to play")
                    .font(.caption.bold())
                    .foregroundStyle(.white.opacity(0.85))
                    .padding(.horizontal, 10).padding(.vertical, 4)
                    .background(Capsule().fill(.black.opacity(0.55)))
            }
        }
        .task(id: url) {
            poster = await ThumbnailService.shared.thumbnail(for: url, size: 1024)
        }
    }
}

/// Audio: poster + Play button. Same NSViewRepresentable workaround as VideoPreview.
private struct AudioPreview: View {
    let url: URL

    var body: some View {
        VStack(spacing: 24) {
            Spacer()
            Image(systemName: "waveform.circle.fill")
                .font(.system(size: 96, weight: .light))
                .foregroundStyle(Theme.gold)
            Text(url.lastPathComponent)
                .font(.headline)
                .lineLimit(1).truncationMode(.middle)
            Button {
                NSWorkspace.shared.open(url)
            } label: {
                Label("Play in default app", systemImage: "play.fill")
                    .padding(.horizontal, 16).padding(.vertical, 8)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Theme.gold))
                    .foregroundStyle(.black)
                    .font(.callout.bold())
            }
            .buttonStyle(.plain)
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.4))
    }
}

/// PDF preview: render the first page via PDFKit and show it as an
/// NSImage. PDFView wrapped in NSViewRepresentable crashes during
/// SwiftUI's eager switch-branch metadata init on macOS 26. The toolbar
/// hands the file to Preview.app for multi-page browsing.
private struct PDFPreview: View {
    let url: URL
    @State private var pageImage: NSImage?

    var body: some View {
        ZStack {
            Color.black.opacity(0.3)
            if let pageImage {
                Image(nsImage: pageImage)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .padding(20)
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "doc.richtext")
                        .font(.system(size: 64))
                        .foregroundStyle(.secondary)
                    Text("Loading PDF…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .task(id: url) {
            // Off the main thread — PDFDocument open + page render can
            // cost real time on a big PDF.
            pageImage = await Task.detached(priority: .userInitiated) {
                guard let doc = PDFDocument(url: url),
                      let page = doc.page(at: 0) else { return nil as NSImage? }
                let bounds = page.bounds(for: .mediaBox)
                // Cap rendered size at 1600 px so giant scans don't OOM.
                let scale = min(1600 / max(bounds.width, bounds.height), 2.0)
                return page.thumbnail(of: CGSize(
                    width: bounds.width * scale,
                    height: bounds.height * scale
                ), for: .mediaBox)
            }.value
        }
    }
}

/// Generic fallback: renders a 1024 px Quick Look thumbnail as NSImage.
/// Toolbar's "Open with default app" covers full-fidelity preview.
private struct UniversalPreview: View {
    let url: URL
    @State private var img: NSImage?

    var body: some View {
        ZStack {
            Color.black.opacity(0.3)
            if let img {
                Image(nsImage: img)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .padding(20)
            } else {
                VStack(spacing: 10) {
                    Image(systemName: iconForExtension(url.pathExtension))
                        .font(.system(size: 96, weight: .light))
                        .foregroundStyle(.secondary)
                    Text(url.lastPathComponent)
                        .font(.callout)
                        .lineLimit(1).truncationMode(.middle)
                    Text("Open in default app for full preview")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
            }
        }
        .task(id: url) {
            img = await ThumbnailService.shared.thumbnail(for: url, size: 1024)
        }
    }

    private func iconForExtension(_ ext: String) -> String {
        switch ext.lowercased() {
        case "zip", "tar", "gz", "rar", "7z": return "doc.zipper"
        case "txt", "md", "rtf":                return "doc.text"
        case "html", "htm":                     return "doc.richtext"
        case "swift", "py", "js", "ts", "rs", "go", "java", "c", "cpp", "h":
            return "chevron.left.forwardslash.chevron.right"
        case "json", "yaml", "yml", "toml", "xml":
            return "curlybraces.square"
        default:                                return "doc"
        }
    }
}

// Tiny FlowLayout for tag pills (SwiftUI doesn't ship one).
struct FlowLayout: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var width: CGFloat = 0; var height: CGFloat = 0; var rowHeight: CGFloat = 0
        for sv in subviews {
            let s = sv.sizeThatFits(.unspecified)
            if width + s.width > maxWidth {
                height += rowHeight + spacing
                width = 0; rowHeight = 0
            }
            width += s.width + spacing
            rowHeight = max(rowHeight, s.height)
        }
        height += rowHeight
        return CGSize(width: maxWidth, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX; var y = bounds.minY; var rowHeight: CGFloat = 0
        for sv in subviews {
            let s = sv.sizeThatFits(.unspecified)
            if x + s.width > bounds.maxX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            sv.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(s))
            x += s.width + spacing
            rowHeight = max(rowHeight, s.height)
        }
    }
}
