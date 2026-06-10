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
    /// Top vision tags per visible file, batched in one SQL query
    /// per reload so tiles don't re-fire 1000× when 1000 are onscreen.
    @State private var tagsByFile: [Int64: [String]] = [:]
    @State private var searchText: String = ""
    @FocusState private var searchFocused: Bool
    /// Debounce against per-keystroke reloads while CLIP semantic
    /// search is active (~50ms per query).
    @State private var searchDebounce: Task<Void, Never>?
    /// When set, the grid shows photos most-similar to this seed
    /// (CLIP image-embedding cosine).
    @State private var similarSeed: FileRow? = nil
    /// Persisted across launches. Empty string means "no filter" since
    /// AppStorage doesn't support optional bindings cleanly.
    @AppStorage("library.kindFilter") private var kindFilterRaw: String = ""
    private var kindFilter: String? {
        get { kindFilterRaw.isEmpty ? nil : kindFilterRaw }
    }
    @State private var lastSeenVersion: Int = -1
    @State private var lastSeenBatchIndex: Int = -1
    @State private var lastReloadAt: Date = .distantPast
    @State private var selected: FileRow?
    /// Siblings frozen at preview-open time so live-scan updates to
    /// `rows` don't yank the file the user is looking at out of the
    /// nav context (the LIMIT 200 query reorders by scanned_at).
    @State private var previewSiblings: [FileRow] = []
    @State private var bulkRenameSheetOpen: Bool = false
    @State private var pendingRenameCount: Int = 0
    @State private var lastBatchAvailable: Bool = false
    @State private var lastTagBatchAvailable: Bool = false
    @State private var undoStatus: String?

    // Multi-select tag mode (P4).
    @State private var selectMode: Bool = false
    @State private var checkedFileIDs: Set<Int64> = []
    @State private var bulkTagSheetOpen: Bool = false

    private let columns = [
        GridItem(.adaptive(minimum: 160, maximum: 220), spacing: 12)
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider().opacity(0.4)
            if let p = engine.lastProgress,
               p.phase == .discovering || p.phase == .tagging || p.phase == .postScan {
                inFlightHeadline(p)
            }
            // Post-scan stage banner — visible while the user
            // continues browsing what's already loaded.
            if engine.faceClusteringInFlight {
                postScanBanner(
                    icon: "person.2.crop.square.stack",
                    title: "Grouping faces…",
                    detail: "On-device AI is matching faces to people. Cards will appear in the People tab."
                )
            }
            if engine.deepAnalyzeInFlight {
                deepAnalyzeHeadline()
            }
            if let seed = similarSeed {
                similaritySeedBanner(seed)
            }
            if let hint = clipSearchHint {
                clipHintBanner(hint)
            }
            if rows.isEmpty {
                empty
            } else {
                grid
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        // Hidden ⌘F button focuses the search field. SwiftUI doesn't
        // attach keyboardShortcut directly to TextField focus, so this
        // is the standard idiom.
        .background(
            Button("") { searchFocused = true }
                .keyboardShortcut("f", modifiers: .command)
                .opacity(0)
                .frame(width: 0, height: 0)
        )
        .onAppear {
            store.openIfPossible()
            reload()
            refreshBulkState()
        }
        .onChange(of: engine.lastBatch?.batchIndex ?? -1) { _, new in
            if new != lastSeenBatchIndex {
                lastSeenBatchIndex = new
                guard Date().timeIntervalSince(lastReloadAt) >= 1.0 else { return }
                lastReloadAt = Date()
                store.notifyChanged()
                reload()
                refreshBulkState()
            }
        }
        .onChange(of: engine.deepAnalyzeComplete?.processed ?? -1) { _, _ in
            refreshBulkState()
        }
        // Terminal reload: the 1 s batch throttle above can swallow the FINAL
        // scan batch (it advances lastSeenBatchIndex before the throttle guard,
        // so the last batch never triggers a reload if it lands within 1 s).
        // Reloading on any terminal event guarantees the grid ends complete.
        .onChange(of: engine.lastTerminalEventAt) { _, _ in
            store.notifyChanged()
            reload()
            refreshBulkState()
        }
        .onChange(of: searchText) { _, _ in
            similarSeed = nil   // typing exits similarity mode
            // Debounce: cancel any pending reload, schedule a new one.
            // The DB hit + optional CLIP encode is ~50-100ms; without
            // debounce, fast typers stutter their own keystrokes.
            searchDebounce?.cancel()
            searchDebounce = Task { @MainActor in
                try? await Task.sleep(nanoseconds: 200_000_000)
                guard !Task.isCancelled else { return }
                reload()
            }
        }
        .onChange(of: kindFilterRaw) { _, _ in reload() }
        .onChange(of: similarSeed?.id) { _, _ in reload() }
        // The encoder's ORT session takes seconds to build after launch /
        // install; without this the first search silently stays keyword-only.
        .onChange(of: CLIPModelInstaller.shared.textEncoderReady) { _, ready in
            if ready { reload() }
        }
        // Surface the undo outcome (renames or tags, incl. partial/total
        // failures) — it was written to `undoStatus` but never shown, so
        // failures were silent.
        .alert("Undo", isPresented: Binding(
            get: { undoStatus != nil },
            set: { if !$0 { undoStatus = nil } }
        )) {
            Button("OK", role: .cancel) { undoStatus = nil }
        } message: {
            Text(undoStatus ?? "")
        }
    }

    /// Inline post-scan banner used for "Grouping faces…" and
    /// "Writing captions…" while the engine chains stages.
    @ViewBuilder
    private func postScanBanner(icon: String, title: String, detail: String) -> some View {
        HStack(spacing: 10) {
            Image(systemName: icon)
                .foregroundStyle(Theme.ai)
                .font(.callout)
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.callout.bold())
                Text(detail).font(.caption2).foregroundStyle(.secondary)
            }
            Spacer()
            ProgressView().controlSize(.small).tint(Theme.ai)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.ai.opacity(0.08)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.ai.opacity(0.30), lineWidth: 1))
        .padding(.horizontal, 20)
        .padding(.top, 8)
    }

    private enum CLIPSearchHint { case install, preparing }

    /// Non-nil when the user has typed a non-trivial query but semantic
    /// search is degraded to keyword search: either the CLIP text encoder
    /// was never installed (point at Settings), or its files exist and
    /// the ORT session is still compiling after launch / install (the
    /// grid re-runs the search automatically once it's ready).
    private var clipSearchHint: CLIPSearchHint? {
        let trimmed = searchText.trimmingCharacters(in: .whitespaces)
        guard trimmed.count >= 3, similarSeed == nil,
              !CLIPModelInstaller.shared.textEncoderReady,
              !CLIPTextEncoder.shared.isReady else { return nil }
        return CLIPTextEncoder.shared.isInstalled ? .preparing : .install
    }

    /// One-line keyword-fallback banner: missing → points the user at
    /// Settings (doesn't switch tabs; avoids extra wiring), compiling →
    /// spinner while the encoder finishes loading.
    @ViewBuilder
    private func clipHintBanner(_ hint: CLIPSearchHint) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "sparkle.magnifyingglass")
                .foregroundStyle(Theme.ai)
            VStack(alignment: .leading, spacing: 1) {
                Text("Showing keyword matches.")
                    .font(.callout.bold())
                Text(hint == .preparing
                     ? "Preparing semantic search… results will refresh automatically."
                     : "Install CLIP in Settings → AI Models for visual semantic search (\"sunset at the beach\", \"red car\", etc.).")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if hint == .preparing {
                ProgressView().controlSize(.small).tint(Theme.ai)
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.ai.opacity(0.08)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.ai.opacity(0.30), lineWidth: 1))
        .padding(.horizontal, 20)
        .padding(.top, 8)
    }

    /// Banner shown when the user enters similarity-search mode.
    @ViewBuilder
    private func similaritySeedBanner(_ seed: FileRow) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "sparkle.magnifyingglass")
                .foregroundStyle(Theme.ai)
            VStack(alignment: .leading, spacing: 1) {
                Text("Photos similar to \(seed.url.lastPathComponent)")
                    .font(.callout.bold())
                Text("Ranked by visual similarity using on-device CLIP embeddings.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Clear") { similarSeed = nil }
                .buttonStyle(.bordered)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.ai.opacity(0.10)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Theme.ai.opacity(0.4), lineWidth: 1))
        .padding(.horizontal, 20)
        .padding(.top, 8)
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
                Text(String(format: "%.0f files per second", p.filesPerSecond))
                    .foregroundStyle(Theme.gold)
                if let eta = p.etaSeconds, eta > 0 {
                    Text("about \(formatETA(eta)) left")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if p.failed > 0 {
                    Text("\(p.failed) couldn't be read").foregroundStyle(.red)
                }
            }
            .font(.caption.monospacedDigit())
            .help("Memory and resource details are in Settings → Advanced.")
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
                Image(systemName: "wand.and.rays")
                    .foregroundStyle(Theme.ai)
                    .font(.title3)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Deep Analyze running…")
                        .font(.headline)
                    Text("On-device AI is captioning images and proposing smart filenames.")
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
        VStack(alignment: .leading, spacing: 12) {
            // Title row — matches the rhythm of every other primary tab.
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text("Library").font(.largeTitle.bold())
                Text("\(rows.count) of \(store.totalFiles)")
                    .font(.callout.monospacedDigit())
                    .foregroundStyle(.secondary)
                Spacer()
            }
            // Action row — search, filter, bulk actions.
            HStack(alignment: .center, spacing: 12) {
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                    TextField("Search filenames, captions, tags, people, text in photos…",
                               text: $searchText)
                        .textFieldStyle(.plain)
                        .frame(minWidth: 220)
                        .focused($searchFocused)
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

            // P4 — multi-select mode toggle. Tiles render with check-
            // boxes; "Tag selected" + "Done" appear in place of the
            // normal action set.
            if selectMode {
                Text("\(checkedFileIDs.count) selected")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Button {
                    if !checkedFileIDs.isEmpty { bulkTagSheetOpen = true }
                } label: {
                    Label("Tag selected", systemImage: "tag.fill")
                        .font(.caption.bold())
                        .padding(.horizontal, 10).padding(.vertical, 5)
                        .background(Capsule().fill(Theme.gold))
                        .foregroundStyle(.black)
                }
                .buttonStyle(.plain)
                .disabled(checkedFileIDs.isEmpty)
                Button("Done") {
                    selectMode = false
                    checkedFileIDs.removeAll()
                }
                .buttonStyle(.bordered)
                .keyboardShortcut(.escape, modifiers: [])
            } else {
                Button {
                    selectMode = true
                } label: {
                    Label("Select", systemImage: "checkmark.square")
                        .font(.caption.bold())
                        .padding(.horizontal, 10).padding(.vertical, 5)
                        .background(Capsule().stroke(.secondary.opacity(0.5), lineWidth: 1))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Enter multi-select mode to apply tags to many files at once")
            }

            // Bulk-rename trigger lives in the Deep Analyze tab now —
            // that's where smart names come from in the workflow. Keep
            // only the per-row "Undo last rename" affordance here.
            if lastBatchAvailable {
                Button(action: undoLastBatch) {
                    Label("Undo last rename", systemImage: "arrow.uturn.backward")
                        .font(.caption.bold())
                        .padding(.horizontal, 10).padding(.vertical, 5)
                        .background(Capsule().stroke(Theme.gold, lineWidth: 1))
                        .foregroundStyle(Theme.gold)
                }
                .buttonStyle(.plain)
                .help("Reverse the most recent rename batch")
            }

            if lastTagBatchAvailable {
                Button(action: undoLastTagBatch) {
                    Label("Undo last tags", systemImage: "arrow.uturn.backward")
                        .font(.caption.bold())
                        .padding(.horizontal, 10).padding(.vertical, 5)
                        .background(Capsule().stroke(Theme.gold, lineWidth: 1))
                        .foregroundStyle(Theme.gold)
                }
                .buttonStyle(.plain)
                .help("Remove only the tags FileID added in the most recent bulk tag batch")
            }

            }
        }
        .padding(20)
        .sheet(isPresented: $bulkRenameSheetOpen, onDismiss: refreshBulkState) {
            BulkRenameSheet(store: store)
        }
        .sheet(isPresented: $bulkTagSheetOpen) {
            BulkTagSheet(
                files: rows.filter { checkedFileIDs.contains($0.id) },
                store: store,
                onComplete: {
                    selectMode = false
                    checkedFileIDs.removeAll()
                }
            )
        }
    }

    private func refreshBulkState() {
        pendingRenameCount = store.filesWithProposedNames(limit: 5000).count
        lastBatchAvailable = (BulkRenameSheet.loadLastBatch()?.isEmpty == false)
        lastTagBatchAvailable = (BulkTagSheet.loadLastBatch()?.isEmpty == false)
    }

    private func undoLastBatch() {
        guard let batch = BulkRenameSheet.loadLastBatch(), !batch.isEmpty else { return }
        let storeRef = store
        Task.detached(priority: .userInitiated) {
            let result = storeRef.undoRenames(batch)
            await MainActor.run {
                if result.failed == 0 && result.skipped == 0 {
                    BulkRenameSheet.clearLastBatch()
                }
                undoStatus = "Reverted \(result.undone) rename\(result.undone == 1 ? "" : "s")"
                    + (result.skipped > 0 ? " · skipped \(result.skipped)" : "")
                    + (result.failed > 0 ? " · failed \(result.failed)" : "")
                refreshBulkState()
                reload()
            }
        }
    }

    private func undoLastTagBatch() {
        guard let batch = BulkTagSheet.loadLastBatch(), !batch.isEmpty else { return }
        let storeRef = store
        Task.detached(priority: .userInitiated) {
            let result = TagWriter.undoBulkAdd(batch)
            await MainActor.run {
                if result.failed == 0 {
                    BulkTagSheet.clearLastBatch()
                }
                undoStatus = "Removed tags from \(result.undone) file\(result.undone == 1 ? "" : "s")"
                    + (result.failed > 0 ? " · failed \(result.failed)" : "")
                    + (result.firstError.map { " — \($0)" } ?? "")
                refreshBulkState()
                storeRef.notifyChanged()
                reload()
            }
        }
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
                    withAnimation(.easeInOut(duration: 0.15)) {
                        kindFilterRaw = k.value ?? ""
                    }
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
                    FileTile(row: row, store: store,
                             selectMode: selectMode,
                             isChecked: checkedFileIDs.contains(row.id),
                             topTags: tagsByFile[row.id] ?? [])
                        .onTapGesture {
                            if selectMode {
                                if checkedFileIDs.contains(row.id) {
                                    checkedFileIDs.remove(row.id)
                                } else {
                                    checkedFileIDs.insert(row.id)
                                }
                            } else {
                                previewSiblings = rows
                                selected = row
                            }
                        }
                        .contextMenu {
                            Button {
                                similarSeed = row
                            } label: {
                                Label("Find similar photos", systemImage: "sparkle.magnifyingglass")
                            }
                            Button {
                                NSWorkspace.shared.activateFileViewerSelecting([row.url])
                            } label: {
                                Label("Show in Finder", systemImage: "folder")
                            }
                        }
                        .background(
                            RoundedRectangle(cornerRadius: Theme.Radius.m)
                                .stroke(selected?.id == row.id && !selectMode
                                        ? Theme.gold : Color.clear,
                                        lineWidth: 2)
                        )
                        // Tiles fade + scale in so the grid "fills
                        // the room" during a live scan instead of
                        // popping items.
                        .transition(.asymmetric(
                            insertion: .opacity.combined(with: .scale(scale: 0.96)),
                            removal: .opacity
                        ))
                }
            }
            .padding(20)
            // Animate on rows.count, not rows.map(\.id) — the map
            // allocates a new array per render and adds up at 1000+ tiles.
            .animation(.easeOut(duration: 0.30), value: rows.count)
        }
        .sheet(item: $selected) { file in
            FilePreviewSheet(file: file, store: store, engine: engine,
                              siblings: previewSiblings, onSelect: { selected = $0 })
        }
    }

    @ViewBuilder
    private var empty: some View {
        if store.totalFiles == 0 {
            EmptyStateView(
                icon: "arrow.left.circle",
                title: "Ready when you are",
                message: "Click Start Scan in the sidebar to begin."
            )
        } else {
            EmptyStateView(
                icon: "magnifyingglass",
                title: "No matches",
                message: "Try a different search or clear the filter."
            )
        }
    }

    // MARK: - Data

    private func reload() {
        defer {
            // Batch chip tags for every visible tile in one SQL
            // query — was N+1 across the grid before.
            tagsByFile = store.topVisionTagsBulk(
                forFileIDs: rows.map { $0.id }, limit: 2
            )
        }
        if let seed = similarSeed {
            rows = store.similarFiles(toFileID: seed.id, limit: 60)
            return
        }
        // CLIP text→image semantic search when the encoder is
        // installed and the query is non-trivial; otherwise fall
        // through to keyword search.
        let trimmed = searchText.trimmingCharacters(in: .whitespaces)
        if trimmed.count >= 3, CLIPTextEncoder.shared.isReady,
           let semantic = store.semanticSearch(query: trimmed, limit: 60),
           !semantic.isEmpty {
            rows = semantic
            return
        }
        rows = store.files(search: searchText, kindFilter: kindFilter)
    }
}

// MARK: - One tile

struct FileTile: View {
    let row: FileRow
    let store: ReadStore
    var selectMode: Bool = false
    var isChecked: Bool = false
    /// Top vision tags injected from the parent's batch query —
    /// avoids N+1 SQL across visible tiles.
    var topTags: [String] = []

    @State private var thumb: NSImage?
    @State private var hovering = false
    @State private var finderTags: [String] = []

    /// Shorten Vision's hierarchical labels for the chip ("animal_water_aquatic"
    /// → "Aquatic", "Year_2024" → "2024"). Last underscore segment wins,
    /// with first letter capitalized. Multi-word labels added by `extraTags`
    /// like "Has Faces" pass through unchanged.
    private static func formatTag(_ raw: String) -> String {
        if raw.contains(" ") { return raw }   // pre-formatted (Has Faces, etc.)
        let last = raw.split(separator: "_").last.map(String.init) ?? raw
        let withSpaces = last.replacingOccurrences(of: "-", with: " ")
        guard let first = withSpaces.first else { return withSpaces }
        return first.uppercased() + withSpaces.dropFirst()
    }

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

    // tagNamesKey carries names only (no colors, per the v1.0 decision),
    // and String.hashValue is seeded per launch — FNV-1a keeps each tag's
    // dot color stable across launches and tiles.
    private static let dotPalette: [Color] = [Theme.gold, Theme.ai, Theme.info, Theme.delight]

    private static func dotColor(for tag: String) -> Color {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in tag.lowercased().utf8 {
            hash = (hash ^ UInt64(byte)) &* 0x0000_0100_0000_01b3
        }
        return dotPalette[Int(hash % UInt64(dotPalette.count))]
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // 1:1 carrier + overlay image so portrait/landscape
            // sources stay aligned. Thumbs crossfade instead of
            // popping; hover lifts via elevation.
            Color.white.opacity(0.04)
                .aspectRatio(1, contentMode: .fit)
                .overlay(thumbContent)
                .clipShape(RoundedRectangle(cornerRadius: 10))
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(Color.white.opacity(hovering ? 0.18 : 0.08),
                                lineWidth: 1)
                )
                .shadow(color: .black.opacity(hovering ? 0.45 : 0.18),
                        radius: hovering ? 14 : 5, x: 0, y: hovering ? 6 : 3)
                .scaleEffect(hovering ? 1.012 : 1.0)
                .animation(.easeOut(duration: 0.18), value: hovering)
                .animation(.easeOut(duration: 0.40), value: thumb != nil)
                .onHover { hovering = $0 }
                .overlay(badgeOverlay)
                .overlay(selectionOverlay)

            // Filename row. When a smart name exists we show
            //   IMG_5512.jpg → Mia at Beach.jpg
            // so the user sees both the current name and what Deep Analyze
            // proposes as a single line. Click anywhere on the tile (existing
            // behavior) opens the preview where Apply lives.
            if let suggested = row.vlmProposedName, !suggested.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    Text(row.url.lastPathComponent)
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .strikethrough(true, color: .secondary.opacity(0.6))
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    HStack(spacing: 3) {
                        Image(systemName: "wand.and.rays")
                            .font(.system(size: 9))
                            .foregroundStyle(Theme.gold)
                        Text("\(suggested).\(row.extension)")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Theme.gold)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .help("Click to apply the smart name. Original name shown crossed out.")
            } else {
                Text(row.url.lastPathComponent)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            // Vision tag chips — at-a-glance content cues. Informational,
            // not actionable, so they use a neutral secondary tint
            // (gold is reserved for primary actions + the Smart name
            // result). Up to 2 highest-confidence labels.
            // Uses .caption2 (semantic, scales with Dynamic Type) instead
            // of fixed .system(size: 9) for accessibility.
            if !topTags.isEmpty {
                HStack(spacing: 3) {
                    ForEach(topTags.prefix(2), id: \.self) { tag in
                        Text(Self.formatTag(tag))
                            .font(.caption2.weight(.medium))
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 5).padding(.vertical, 2)
                            .background(
                                RoundedRectangle(cornerRadius: 4)
                                    .fill(Color.secondary.opacity(0.10))
                            )
                            .lineLimit(1)
                    }
                    Spacer(minLength: 0)
                }
            }
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
        // Keyed on store.version so tag edits / bulk undo refresh the
        // Finder-tag dots in place (thumbnail re-fetches are NSCache hits).
        .task(id: "\(row.id)·\(store.version)") {
            thumb = await ThumbnailService.shared.thumbnail(for: row.url, size: 264)
            // Off-main like FinderTagsEditor — xattr reads can stall on
            // slow / network volumes.
            let url = row.url
            finderTags = await Task.detached { TagWriter.readTags(at: url) }.value
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityDescription)
        .accessibilityAddTraits(.isButton)
        .accessibilityHint("Opens the file preview")
    }

    @ViewBuilder
    private var selectionOverlay: some View {
        if selectMode {
            ZStack(alignment: .topLeading) {
                if isChecked {
                    RoundedRectangle(cornerRadius: 10)
                        .fill(Theme.gold.opacity(0.18))
                        .overlay(
                            RoundedRectangle(cornerRadius: 10)
                                .stroke(Theme.gold, lineWidth: 3)
                        )
                }
                Image(systemName: isChecked ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 22))
                    .foregroundStyle(isChecked ? Theme.gold : Color.white.opacity(0.85))
                    .background(Circle().fill(.black.opacity(0.45)))
                    .padding(8)
            }
        }
    }

    /// Spoken description for VoiceOver: filename, kind, optional date,
    /// face indicator, OCR indicator, tag count.
    private var accessibilityDescription: String {
        var parts: [String] = []
        parts.append(row.url.lastPathComponent)
        parts.append(row.kind.capitalized)
        if let date = row.displayDate {
            parts.append(date.formatted(date: .abbreviated, time: .omitted))
        }
        if row.hasFaces { parts.append("contains faces") }
        if row.hasText { parts.append("contains text") }
        if !finderTags.isEmpty {
            parts.append("\(finderTags.count) Finder tag\(finderTags.count == 1 ? "" : "s")")
        }
        return parts.joined(separator: ", ")
    }

    @ViewBuilder
    private var thumbContent: some View {
        if let thumb {
            Image(nsImage: thumb)
                .resizable()
                .scaledToFill()
                .transition(.opacity)
        } else if row.kind == "image" {
            // Show a shimmer while the thumbnail loads — feels more
            // alive than a static placeholder, signals "something is
            // arriving".
            ShimmerView(cornerRadius: 10)
        } else {
            // Non-image kinds get the icon placeholder (no thumb pending).
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

            // OCR-text / Finder-tag indicators top-right. (The "Faces"
            // badge was removed for Windows/macOS lockstep — faces surface
            // in the People tab; the badge read as noise on a Library tile.)
            VStack(alignment: .trailing, spacing: 4) {
                if row.hasText {
                    Image(systemName: "text.viewfinder")
                        .font(.system(size: 13))
                        .foregroundStyle(.white, .black.opacity(0.6))
                        .help("OCR text available")
                }
                if !finderTags.isEmpty {
                    HStack(spacing: -3) {
                        ForEach(finderTags.prefix(3), id: \.self) { tag in
                            Circle()
                                .fill(Self.dotColor(for: tag))
                                .frame(width: 8, height: 8)
                                .overlay(Circle().stroke(.black.opacity(0.5), lineWidth: 1))
                        }
                    }
                    .help("Finder tags: \(finderTags.joined(separator: ", "))")
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
                                                Text("Smart name")
                                                    .font(.caption.bold())
                                                    .foregroundStyle(.secondary)
                                                Text("\(proposed).\(file.extension)")
                                                    .font(.caption.monospaced())
                                                    .foregroundStyle(Theme.gold)
                                            }
                                            Spacer()
                                            Button("Apply") {
                                                let oldPath = file.pathText
                                                if let newURL = store.applyProposedName(file: file),
                                                   newURL.path != oldPath {
                                                    // P6 — record the single-file rename so the
                                                    // Library "Undo last rename" button can revert
                                                    // it. Uses the same UserDefaults slot the
                                                    // bulk-rename sheet writes to.
                                                    let outcome = ReadStore.RenameOutcome(
                                                        fileID: file.id,
                                                        oldPath: oldPath,
                                                        newPath: newURL.path
                                                    )
                                                    BulkRenameSheet.saveLastBatch([outcome])
                                                }
                                                dismiss()
                                            }
                                            .buttonStyle(.bordered)
                                            .help("Renames the file on disk and updates the library row. Undo from the Library header if you change your mind.")
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
                        FinderTagsEditor(file: file, store: store)
                    }
                    .padding(16)
                }
                .frame(width: 360)
            }
        }
        .frame(minWidth: 960, minHeight: 600)
        .background(LavaLampBackground())
        .preferredColorScheme(.dark)
        .focusable()
        .focusEffectDisabled()
        // Sheet-level key handler — beats Button.keyboardShortcut for
        // arrows because text fields inside the sheet (tag editor) can
        // steal focus from the buttons. .onKeyPress runs whenever the
        // sheet's focus subtree handles a key.
        .onKeyPress(.leftArrow) {
            step(-1); return .handled
        }
        .onKeyPress(.rightArrow) {
            step(1); return .handled
        }
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

// MARK: - Finder tags editor

/// Inline tag editor — reads the file's current macOS Finder tags
/// (URLResourceKey.tagNamesKey), shows them as pills, lets the user
/// add new tags via a text field. Writes go straight to the file via
/// TagWriter so they show up everywhere macOS exposes Finder tags
/// (Finder sidebar, Spotlight, Smart Folders).
private struct FinderTagsEditor: View {
    let file: FileRow
    let store: ReadStore
    @State private var tags: [String] = []
    @State private var draft: String = ""
    @State private var error: String?

    var body: some View {
        GlassCard {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    Image(systemName: "tag.fill")
                        .foregroundStyle(Theme.gold)
                    Text("Finder tags").font(.headline)
                    Spacer()
                    Text("(visible in Finder + Spotlight)")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
                if tags.isEmpty {
                    Text("None yet — add a tag below.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    FlowLayout(spacing: 6) {
                        ForEach(tags, id: \.self) { tag in
                            tagPill(tag)
                        }
                    }
                }
                HStack(spacing: 6) {
                    TextField("Add tag…", text: $draft, onCommit: addDraft)
                        .textFieldStyle(.roundedBorder)
                        .font(.caption)
                    Button(action: addDraft) {
                        Image(systemName: "plus.circle.fill")
                            .foregroundStyle(Theme.gold)
                    }
                    .buttonStyle(.plain)
                    .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
                }
                if let e = error {
                    Text(e).font(.caption2).foregroundStyle(.orange)
                }
            }
        }
        .task(id: file.id) { reload() }
    }

    private func reload() {
        // Read xattr off the main thread — for files on slow / network
        // volumes the read can stall the preview sheet for hundreds of
        // milliseconds while the user is trying to scrub through.
        let url = file.url
        Task.detached {
            let result = TagWriter.readTags(at: url)
            await MainActor.run {
                tags = result
                error = nil
            }
        }
    }

    private func addDraft() {
        let trimmed = draft.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        do {
            tags = try TagWriter.addTags([trimmed], at: file.url)
            draft = ""
            error = nil
            store.notifyChanged()  // refresh Library tile tag-count
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func remove(_ tag: String) {
        do {
            tags = try TagWriter.removeTags([tag], at: file.url)
            error = nil
            store.notifyChanged()
        } catch {
            self.error = error.localizedDescription
        }
    }

    @ViewBuilder
    private func tagPill(_ tag: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: "tag.fill")
                .font(.system(size: 9))
            Text(tag).font(.caption)
            Button {
                remove(tag)
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Remove this tag")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(Capsule().fill(Theme.gold.opacity(0.15)))
        .overlay(Capsule().stroke(Theme.gold.opacity(0.3), lineWidth: 1))
        .foregroundStyle(Theme.gold)
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
