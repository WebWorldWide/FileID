// Library: DB-backed thumbnail grid. FTS5 search (OCR + filename) +
// kind filter; re-queries on each batchSummary event for live fill-in.
import SwiftUI
import AVKit
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
    /// In-flight off-main reload. Cancelled before each new reload so a
    /// slow query can't land its rows after a newer one (latest-wins).
    @State private var reloadTask: Task<Void, Never>?
    /// Tile-refresh epoch. `store.version` bumps on every change including
    /// the per-second counter refresh during a live scan; keying each
    /// visible tile's xattr/thumbnail `.task` on it re-reads Finder tags
    /// for unchanged files every tick. Freeze the epoch during a scan —
    /// Finder xattrs can't change while the engine only writes DB rows —
    /// so tiles don't re-task; the terminal reload bumps `store.version`
    /// once at the end to refresh them. Kept in sync with `store.version`
    /// whenever no scan is active.
    @State private var frozenTileVersion: Int = 0
    /// Edit-only epoch, bumped by in-app tag / undo writes. Unlike
    /// `frozenTileVersion` it is NOT frozen during a scan, so a Finder-tag
    /// edit, bulk-tag, or undo performed while the engine is scanning still
    /// refreshes the affected tiles' dots instead of going stale until the
    /// scan ends. Combined with the counter epoch in `tileRefreshToken`.
    @State private var editEpoch: Int = 0
    /// When set, the grid shows photos most-similar to this seed
    /// (CLIP image-embedding cosine).
    @State private var similarSeed: FileRow? = nil
    @State private var visualSearchPending = false
    /// Persisted across launches. Empty string means "no filter" since
    /// AppStorage doesn't support optional bindings cleanly.
    @AppStorage("library.kindFilter") private var kindFilterRaw: String = ""
    private var kindFilter: String? {
        get { kindFilterRaw.isEmpty ? nil : kindFilterRaw }
    }
    @State private var lastSeenBatchIndex: Int = -1
    @State private var lastReloadAt: Date = .distantPast
    /// Drives the preview sheet: which file is shown is keyed on its id
    /// (not a raw `FileRow`) so navigation mutates the displayed file in
    /// place — the sheet keeps identity instead of dismissing+re-presenting.
    @State private var previewSelectedID: Int64? = nil
    @State private var paneSelectedID: Int64? = nil
    /// Keyword and plain listings page in from the database as the grid
    /// nears its end; similarity and semantic results are one ranked list.
    @State private var canLoadMore = false
    @State private var loadingMore = false
    private static let pageSize = 300
    /// The tile about two screens before the end; the grid creates it lazily,
    /// so its appearance means the user is approaching the last loaded row.
    private var loadTriggerID: Int64? { rows.isEmpty ? nil : rows[max(0, rows.count - 24)].id }
    private var paneRow: FileRow? { selectMode ? nil : rows.first { $0.id == paneSelectedID } }
    /// Siblings frozen at preview-open time so live-scan updates to
    /// `rows` don't yank the file the user is looking at out of the
    /// nav context (the LIMIT 200 query reorders by scanned_at).
    @State private var previewSiblings: [FileRow] = []
    /// In-flight full-library sibling upgrade for the open preview. Cancelled
    /// before each new `openPreview` and on dismiss so rapid open/close or
    /// fast photo-switching can't pile up concurrent 1M-row fetches.
    @State private var previewSiblingTask: Task<Void, Never>? = nil
    @State private var bulkRenameSheetOpen: Bool = false
    @State private var lastBatchAvailable: Bool = false
    @State private var lastTagBatchAvailable: Bool = false
    @State private var undoStatus: String?

    // Multi-select tag mode.
    @State private var selectMode: Bool = false
    // id→FileRow (not a Set) so a selected file survives a `rows` reload
    // (live-scan throttle / filter change re-fetches only the top-200 window);
    // bulk-tag then targets every checked file, not just those still on screen.
    @State private var checkedFiles: [Int64: FileRow] = [:]
    @State private var bulkTagSheetOpen: Bool = false

    private let columns = [
        GridItem(.adaptive(minimum: 160, maximum: 220), spacing: 12)
    ]

    private var scanActive: Bool {
        guard let p = engine.lastProgress else { return false }
        return p.phase == .discovering || p.phase == .tagging || p.phase == .postScan
    }

    /// The composite id each tile's `.task` keys on. The counter epoch is
    /// frozen during a scan (see `frozenTileVersion`) so the per-second batch
    /// bump doesn't re-task every visible tile; the edit epoch is never frozen
    /// so an in-app tag / undo write still refreshes the dots mid-scan. Either
    /// component changing re-tasks the affected tiles.
    private var tileRefreshToken: String {
        "\(scanActive ? frozenTileVersion : store.version)·\(editEpoch)"
    }

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
                HStack(spacing: 0) {
                    grid.frame(maxWidth: .infinity, maxHeight: .infinity)
                    if let row = paneRow {
                        VHairline()
                        LibrarySelectedPane(row: row, store: store, engine: engine,
                                            onOpen: { openPreview(row) },
                                            onFindSimilar: { similarSeed = row },
                                            onClose: { paneSelectedID = nil })
                            .frame(width: 300)
                            .transition(.move(edge: .trailing).combined(with: .opacity))
                    }
                }
                .animation(Theme.spring, value: paneRow == nil)
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
            refreshOpenPreview()
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
        // Keep the tile-refresh epoch tracking store.version while idle so a
        // tag edit / undo refreshes the dots, but leave it frozen during a
        // scan (the per-second counter bump must not re-task every tile).
        .onChange(of: store.version) { _, v in
            if !scanActive { frozenTileVersion = v }
        }
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
                .font(Theme.Font.body(13.5))
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(Theme.Font.body(13.5, .semibold))
                Text(detail).font(Theme.Font.body(11)).foregroundStyle(.secondary)
            }
            Spacer()
            ProgressView().controlSize(.small).tint(Theme.ai)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.ai.opacity(0.08)))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.ai.opacity(0.30), lineWidth: 1))
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
        if visualSearchPending { return .preparing }
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
                Text(similarSeed == nil ? "Showing keyword matches." : "Finding similar files.")
                    .font(Theme.Font.body(13.5, .semibold))
                Text(hint == .preparing
                     ? "Preparing semantic search… results will refresh automatically."
                     : "Install CLIP in Settings → AI Models for visual semantic search (\"sunset at the beach\", \"red car\", etc.).")
                    .font(Theme.Font.body(11))
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if hint == .preparing {
                ProgressView().controlSize(.small).tint(Theme.ai)
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.ai.opacity(0.08)))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.ai.opacity(0.30), lineWidth: 1))
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
                    .font(Theme.Font.body(13.5, .semibold))
                Text("Ranked by visual similarity using on-device CLIP embeddings.")
                    .font(Theme.Font.body(11))
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Clear") { similarSeed = nil }
                .buttonStyle(.dsSecondary)
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.ai.opacity(0.10)))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.ai.opacity(0.4), lineWidth: 1))
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
                    .font(Theme.Font.body(16, .semibold))
                VStack(alignment: .leading, spacing: 1) {
                    Text(p.phase == .discovering ? "Discovering files…"
                         : p.phase == .tagging ? "Tagging files…" : "Post-scan…")
                        .font(Theme.Font.body(14, .semibold))
                    Text("Tags + face detection. Smart names come later via Deep Analyze.")
                        .font(Theme.Font.body(11))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Text(p.total > 0
                     ? "\(p.processed) / \(p.total)  ·  \(p.discovered) found"
                     : "\(p.processed) processed  ·  \(p.discovered) found")
                    .font(Theme.Font.body(13.5).monospacedDigit())
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
                    Text("\(p.failed) couldn't be read").foregroundStyle(Theme.danger)
                }
            }
            .font(Theme.Font.body(12).monospacedDigit())
            .help("Memory and resource details are in Settings → Advanced.")
        }
        .padding(16)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                .fill(.ultraThinMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
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
                    .font(Theme.Font.body(16, .semibold))
                VStack(alignment: .leading, spacing: 1) {
                    Text("Deep Analyze running…")
                        .font(Theme.Font.body(14, .semibold))
                    Text("On-device AI is captioning images and proposing smart filenames.")
                        .font(Theme.Font.body(11))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if let p {
                    Text("\(p.processed) / \(p.total)")
                        .font(Theme.Font.body(13.5).monospacedDigit())
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
                .font(Theme.Font.body(12).monospacedDigit())
                .foregroundStyle(.tertiary)
            }
            if let last {
                Divider().opacity(0.3)
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: "wand.and.rays")
                        .foregroundStyle(Theme.gold)
                        .font(Theme.Font.body(13.5))
                    VStack(alignment: .leading, spacing: 4) {
                        if let n = last.proposedName {
                            Text(n)
                                .font(Theme.Font.mono(12.5, .semibold))
                                .foregroundStyle(Theme.gold)
                        }
                        Text(last.description)
                            .font(Theme.Font.body(12))
                            .foregroundStyle(.secondary)
                            .lineLimit(3)
                    }
                    Spacer()
                }
            }
        }
        .padding(16)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                .fill(.ultraThinMaterial)
                .overlay(
                    RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
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
            HStack(alignment: .center, spacing: 10) {
                HStack(spacing: 8) {
                    Image(systemName: "line.3.horizontal.decrease").font(.system(size: 12, weight: .semibold)).foregroundStyle(Theme.text3)
                    TextField("Filter by name, tag, caption or person", text: $searchText)
                        .textFieldStyle(.plain)
                        .font(Theme.Font.body(13.5))
                        .focused($searchFocused)
                    if !searchText.isEmpty {
                        Button { searchText = "" } label: { Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(Theme.text3) }
                            .buttonStyle(.plain)
                            .help("Clear search")
                            .accessibilityLabel("Clear search")
                    }
                }
                .padding(.horizontal, 10)
                .frame(height: Theme.Metrics.controlLarge)
                .frame(minWidth: 140, maxWidth: 320)
                .layoutPriority(1)
                .background(Color.black.opacity(0.3), ignoresSafeAreaEdges: [])
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong.opacity(1.15), lineWidth: 1))

                kindPicker

                ViewThatFits(in: .horizontal) {
                    MetaText("\(rows.count.formatted()) of \(store.totalFiles.formatted()) shown", size: 11).fixedSize()
                    MetaText("\(rows.count.formatted())/\(store.totalFiles.formatted())", size: 11).fixedSize()
                    Color.clear.frame(width: 0, height: 0)
                }
                .fixedSize(horizontal: false, vertical: true)

                Spacer(minLength: 8)

            // multi-select mode toggle. Tiles render with check-
            // boxes; "Tag selected" + "Done" appear in place of the
            // normal action set.
            if selectMode {
                MetaText("\(checkedFiles.count) selected", size: 11)
                Button {
                    if !checkedFiles.isEmpty { bulkTagSheetOpen = true }
                } label: { Label("Tag selected", systemImage: "tag.fill") }
                .buttonStyle(DSPrimaryButtonStyle(height: 28))
                .disabled(checkedFiles.isEmpty)
                Button("Done") {
                    selectMode = false
                    checkedFiles.removeAll()
                }
                .buttonStyle(DSSecondaryButtonStyle(height: 28))
                .keyboardShortcut(.escape, modifiers: [])
            } else {
                Button { selectMode = true } label: { Label("Select", systemImage: "checkmark.square").fixedSize() }
                .buttonStyle(DSSecondaryButtonStyle(height: 28))
                .fixedSize()
                .help("Enter multi-select mode to apply tags to many files at once")
            }

            // Bulk-rename trigger lives in the Deep Analyze tab now —
            // that's where smart names come from in the workflow. Keep
            // only the per-row "Undo last rename" affordance here.
            if lastBatchAvailable {
                Button(action: undoLastBatch) { Label("Undo last rename", systemImage: "arrow.uturn.backward") }
                .buttonStyle(DSAccentOutlineButtonStyle(height: 28))
                .help("Reverse the most recent rename batch")
            }

            if lastTagBatchAvailable {
                Button(action: undoLastTagBatch) { Label("Undo last tags", systemImage: "arrow.uturn.backward") }
                .buttonStyle(DSAccentOutlineButtonStyle(height: 28))
                .help("Remove only the tags FileID added in the most recent bulk tag batch")
            }

            }
        }
        .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.top, Theme.Metrics.pageInsetY).padding(.bottom, 12)
        .onAppear {
            if let handoff = UserDefaults.standard.string(forKey: "libraryQueryHandoff"), !handoff.isEmpty {
                searchText = handoff
                UserDefaults.standard.removeObject(forKey: "libraryQueryHandoff")
            }
        }
        .sheet(isPresented: $bulkRenameSheetOpen, onDismiss: refreshBulkState) {
            BulkRenameSheet(store: store)
        }
        .sheet(isPresented: $bulkTagSheetOpen) {
            BulkTagSheet(
                files: Array(checkedFiles.values),
                store: store,
                onComplete: {
                    selectMode = false
                    checkedFiles.removeAll()
                    editEpoch += 1   // refresh tile dots even mid-scan
                }
            )
        }
    }

    private func refreshBulkState() {
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
                editEpoch += 1   // refresh tiles (renamed files) even mid-scan
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
                if result.failed == 0 && result.skipped == 0 {
                    BulkTagSheet.clearLastBatch()
                }
                undoStatus = "Removed tags from \(result.undone) file\(result.undone == 1 ? "" : "s")"
                    + (result.skipped > 0 ? " · skipped \(result.skipped)" : "")
                    + (result.failed > 0 ? " · failed \(result.failed)" : "")
                    + (result.firstError.map { " — \($0)" } ?? "")
                editEpoch += 1   // refresh tile dots even mid-scan
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
        HStack(spacing: 0) {
            ForEach(Array(kinds.enumerated()), id: \.offset) { _, k in
                let active = kindFilter == k.value
                Button {
                    withAnimation(Theme.spring) {
                        kindFilterRaw = k.value ?? ""
                    }
                } label: {
                    Text(k.label)
                        .font(Theme.Font.body(12.5, active ? .semibold : .medium))
                        .foregroundStyle(active ? Theme.onGold : Theme.text2)
                        .lineLimit(1).fixedSize()
                        .padding(.horizontal, 12)
                        .frame(height: Theme.Metrics.control)
                        .background(active ? Theme.gold : Color.clear, ignoresSafeAreaEdges: [])
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(active ? .isSelected : [])
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
    }

    // MARK: - Grid

    @ViewBuilder
    private var grid: some View {
        LibraryCollectionGrid(
            rows: rows,
            tagsByFile: tagsByFile,
            selectMode: selectMode,
            checkedIDs: Set(checkedFiles.keys),
            selectedID: paneSelectedID ?? previewSelectedID,
            refreshToken: tileRefreshToken,
            onSelect: { paneSelectedID = $0.id },
            onOpen: { openPreview($0) },
            onToggleCheck: { row in
                if checkedFiles[row.id] != nil { checkedFiles[row.id] = nil } else { checkedFiles[row.id] = row }
            },
            onFindSimilar: { similarSeed = $0 },
            onNearEnd: { loadMore() })
        .overlay(alignment: .bottom) {
            if loadingMore { ProgressView().controlSize(.small).padding(.bottom, 12) }
        }
        // One stable sheet. `isPresented` stays true across navigation so the
        // sheet keeps identity; the displayed file is driven by selectedID.
        .sheet(isPresented: Binding(
            get: { previewSelectedID != nil },
            set: {
                if !$0 {
                    previewSelectedID = nil
                    previewSiblingTask?.cancel()
                    previewSiblingTask = nil
                }
            }
        )) {
            FilePreviewSheet(siblings: previewSiblings,
                              selectedID: $previewSelectedID,
                              store: store, engine: engine,
                              onEdit: { editEpoch += 1 })
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

    /// Open the full-screen preview for `row`. Opens immediately on the on-screen rows,
    /// then upgrades the prev/next nav context to the FULL library so arrow-nav isn't
    /// capped at the 200-tile grid window (and the frozen list won't churn with the
    /// live-scan `scanned_at` reorder). A search / find-similar result keeps its own set
    /// so arrows stay within those matches.
    private func openPreview(_ row: FileRow) {
        previewSiblings = rows
        previewSelectedID = row.id
        guard searchText.trimmingCharacters(in: .whitespaces).isEmpty, similarSeed == nil else { return }
        let kf = kindFilter
        previewSiblingTask?.cancel()
        previewSiblingTask = Task { @MainActor in
            let full = await store.filesAsync(limit: 1_000_000, kindFilter: kf)
            guard !Task.isCancelled else { return }
            // Upgrade the nav context by id — the user may have arrowed to a
            // different photo while this loaded; keep the upgrade as long as the
            // current selection is still represented in the full set.
            guard let id = previewSelectedID, !full.isEmpty,
                  full.contains(where: { $0.id == id }) else { return }
            previewSiblings = full
        }
    }

    private func refreshOpenPreview() {
        guard let id = previewSelectedID,
              let updated = store.files(forFileIDs: [id]).first,
              let index = previewSiblings.firstIndex(where: { $0.id == id }) else { return }
        previewSiblings[index] = updated
    }

    private func reload() {
        // Off the MainActor: similarity + semantic search cosine-scan the full
        // clip_embeddings table (O(N·512)) and the keyword query hits multiple
        // tables plus a face join — multi-second on a 50k library, and the live
        // scan re-fires this on every throttled batch. Run them on a background
        // task via ReadStore's *Async twins and assign only the results on main.
        // Latest-wins: cancel any in-flight reload so a slow query can never
        // overwrite a newer one's results.
        let seed = similarSeed
        let query = searchText
        let kind = kindFilter
        let encoderReady = CLIPTextEncoder.shared.isReady
        reloadTask?.cancel()
        visualSearchPending = false
        reloadTask = Task { @MainActor in
            defer { if !Task.isCancelled { visualSearchPending = false } }
            var newRows: [FileRow]
            var pageable = false
            if let seed {
                visualSearchPending = true
                if let response = await engine.searchCatalog(seedID: seed.id), response.status == "ok" {
                    newRows = await store.catalogFilesAsync(ids: response.hits.map(\.fileID), kindFilter: kind)
                } else { newRows = [] }
            } else {
                newRows = await store.filesAsync(limit: Self.pageSize, search: query, kindFilter: kind)
                pageable = newRows.count == Self.pageSize
                let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
                if trimmed.count >= 3, encoderReady, let vector = await store.semanticVectorAsync(query: trimmed) {
                    guard !Task.isCancelled else { return }
                    rows = newRows
                    visualSearchPending = true
                    if let response = await engine.searchCatalog(query: trimmed, vector: vector), response.status == "ok" {
                        newRows = await store.catalogFilesAsync(ids: response.hits.map(\.fileID), kindFilter: kind)
                        pageable = false
                    }
                }
            }
            guard !Task.isCancelled else { return }
            // Batch chip tags for every visible tile in one SQL query — was
            // N+1 across the grid before. Off-main too (same read-store scan).
            let ids = newRows.map { $0.id }
            let tags = await Task.detached(priority: .userInitiated) { [store] in
                store.topVisionTagsBulk(forFileIDs: ids, limit: 4)
            }.value
            guard !Task.isCancelled else { return }
            rows = newRows
            tagsByFile = tags
            canLoadMore = pageable
        }
    }

    private func loadMore() {
        guard canLoadMore, !loadingMore, similarSeed == nil else { return }
        loadingMore = true
        let offset = rows.count, query = searchText, kind = kindFilter
        Task { @MainActor in
            defer { loadingMore = false }
            let page = await store.filesAsync(offset: offset, limit: Self.pageSize, search: query, kindFilter: kind)
            guard query == searchText, kind == kindFilter, offset == rows.count else { return }
            let ids = page.map(\.id)
            let tags = await Task.detached(priority: .userInitiated) { [store] in
                store.topVisionTagsBulk(forFileIDs: ids, limit: 4)
            }.value
            let known = Set(rows.map(\.id))
            rows.append(contentsOf: page.filter { !known.contains($0.id) })
            tagsByFile.merge(tags) { _, new in new }
            canLoadMore = page.count == Self.pageSize
        }
    }
}

// MARK: - Preview sheet
//
// Full-bleed preview + metadata panel + tags. Reveal-in-Finder.
private struct FilePreviewSheet: View {
    let siblings: [FileRow]              // for prev/next arrow nav
    @Binding var selectedID: Int64?
    let store: ReadStore
    let engine: EngineClient
    /// Bump the parent's edit epoch so a Finder-tag edit / smart-name apply
    /// made in this sheet refreshes the underlying grid tile even mid-scan.
    var onEdit: () -> Void = {}
    @Environment(\.dismiss) var dismiss
    @State private var preview: NSImage?
    @State private var tags: [String] = []
    @State private var people: [String] = []
    @AppStorage("preview.showInfo") private var showInfo = true
    /// Holds key focus so the arrow-key handlers fire; the tag field's
    /// focus is tracked separately to suppress nav while the user types.
    @FocusState private var keyFocus: Bool
    @FocusState private var tagFieldFocused: Bool

    /// Displayed file + its position, derived from selectedID so navigation
    /// mutates the shown file in place without swapping the sheet's identity.
    private var file: FileRow? { siblings.first { $0.id == selectedID } }
    private var siblingIndex: Int? { siblings.firstIndex { $0.id == selectedID } }

    private func step(_ delta: Int) {
        guard let idx = siblingIndex else { return }
        let t = idx + delta
        guard siblings.indices.contains(t) else { return }
        selectedID = siblings[t].id
    }

    var body: some View {
        Group {
            if let file {
                content(for: file)
            } else {
                Color.clear.onAppear { dismiss() }
            }
        }
    }

    @ViewBuilder
    private func content(for file: FileRow) -> some View {
        VStack(spacing: 0) {
            header(file)
            HStack(spacing: 0) {
                canvas(file)
                if showInfo {
                    VHairline()
                    infoPanel(file)
                        .frame(width: 340)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                }
            }
            .animation(Theme.spring, value: showInfo)
        }
        .frame(minWidth: 1040, idealWidth: 1280, minHeight: 680, idealHeight: 820)
        .background(Color(hex: 0x0C0C0E), ignoresSafeAreaEdges: [])
        .preferredColorScheme(.dark)
        .focusable()
        .focusEffectDisabled()
        .focused($keyFocus)
        // Grab key focus when the sheet appears so the arrow handlers fire
        // without a click; reclaim it whenever the tag field gives focus up.
        // The brief defer lets the sheet enter the responder hierarchy first —
        // setting focus synchronously in .task doesn't reliably stick on a
        // freshly presented sheet.
        .task {
            try? await Task.sleep(for: .milliseconds(50))
            keyFocus = true
        }
        .onChange(of: tagFieldFocused) { _, focused in
            if !focused { keyFocus = true }
        }
        // Sheet-level key handler — beats Button.keyboardShortcut for arrows
        // because focus lives on the sheet, not the nav buttons. Suppressed
        // while the tag field is focused so typing doesn't navigate.
        .onKeyPress(.leftArrow) {
            if tagFieldFocused { return .ignored }
            step(-1); return .handled
        }
        .onKeyPress(.rightArrow) {
            if tagFieldFocused { return .ignored }
            step(1); return .handled
        }
        .task(id: "\(file.id):\(file.vlmAnalyzedAt?.timeIntervalSince1970 ?? 0)") {
            // Generate a larger preview for the sheet (640px). Keyed on
            // file id and analysis timestamp so it re-fires on sibling navigation
            // and after Deep Analyze updates the current row. Clear first so the
            // spinner shows and the prior preview never bleeds into the new state.
            preview = nil
            tags = []
            people = []
            // Read DB tags off the main actor (detached read doesn't inherit
            // `.task` cancellation; drop it once `.task(id:)` superseded us — R7).
            let fileID = file.id
            let loadedTags = await Task.detached { [store] in
                store.tags(forFileID: fileID)
            }.value
            guard !Task.isCancelled else { return }
            tags = loadedTags
            let loadedPeople = await Task.detached { [store] in store.personNames(forFileID: fileID) }.value
            guard !Task.isCancelled else { return }
            people = loadedPeople
            let image = file.kind == "image"
                ? await ThumbnailService.previewImage(for: file.url, maxPixelSize: 2400)
                : await ThumbnailService.shared.thumbnail(for: file.url, size: 1600)
            // ThumbnailService doesn't honor Task cancellation, so a slow load for
            // the prior file (network volume) can resolve AFTER the user arrowed on;
            // drop it once `.task(id:)` superseded us, else it overwrites the current
            // file's image. Mirrors the FinderTagsEditor read guard.
            guard !Task.isCancelled else { return }
            preview = image
        }
    }

    private func header(_ file: FileRow) -> some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(file.url.lastPathComponent)
                    .font(Theme.Font.body(15, .semibold)).foregroundStyle(Theme.text1)
                    .lineLimit(1).truncationMode(.middle)
                Text(file.url.deletingLastPathComponent().path)
                    .font(Theme.Font.mono(11)).foregroundStyle(Theme.text3)
                    .lineLimit(1).truncationMode(.head)
            }
            Spacer(minLength: 12)
            if siblings.count > 1, let index = siblingIndex {
                HStack(spacing: 2) {
                    iconButton("chevron.left", "Previous file (←)") { step(-1) }.disabled(index <= 0)
                    Text("\(index + 1) of \(siblings.count)")
                        .font(Theme.Font.body(12.5)).monospacedDigit().foregroundStyle(Theme.text2)
                        .frame(minWidth: 70)
                    iconButton("chevron.right", "Next file (→)") { step(1) }.disabled(index >= siblings.count - 1)
                }
            }
            if file.kind == "image" { DeepAnalyzeButton(engine: engine, file: file) }
            Button { NSWorkspace.shared.activateFileViewerSelecting([file.url]) } label: { Label("Finder", systemImage: "folder") }
                .buttonStyle(DSSecondaryButtonStyle(height: 28)).help("Show in Finder")
            Button { NSWorkspace.shared.open(file.url) } label: { Label("Open", systemImage: "arrow.up.right.square") }
                .buttonStyle(DSSecondaryButtonStyle(height: 28)).help("Open in the default app")
            iconButton("sidebar.right", showInfo ? "Hide info (⌘I)" : "Show info (⌘I)") { showInfo.toggle() }
                .keyboardShortcut("i", modifiers: .command)
            iconButton("xmark", "Close preview (Esc)") { dismiss() }
                .keyboardShortcut(.escape, modifiers: [])
        }
        .padding(.horizontal, 16)
        .frame(height: 56)
        .background(Theme.bg1, ignoresSafeAreaEdges: [])
        .overlay(alignment: .bottom) { Hairline() }
    }

    private func iconButton(_ symbol: String, _ help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: 12, weight: .semibold)).foregroundStyle(Theme.text2)
                .frame(width: 28, height: 28)
        }
        .buttonStyle(.dsRow)
        .help(help)
        .accessibilityLabel(help.replacingOccurrences(of: #" \([^)]*\)$"#, with: "", options: .regularExpression))
    }

    @ViewBuilder
    private func canvas(_ file: FileRow) -> some View {
        Group {
            switch file.kind {
            case "video": VideoPreview(url: file.url)
            case "audio": AudioPreview(url: file.url)
            case "pdf": PDFPreview(url: file.url)
            case "image":
                ZStack {
                    if let preview {
                        Image(nsImage: preview)
                            .resizable().interpolation(.high)
                            .aspectRatio(contentMode: .fit)
                            .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                            .shadow(color: .black.opacity(0.5), radius: 20, y: 10)
                            .padding(28)
                            .transition(.opacity)
                    } else {
                        ProgressView().controlSize(.small)
                    }
                }
                .animation(Theme.quick, value: preview == nil)
            default: UniversalPreview(url: file.url)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.4), ignoresSafeAreaEdges: [])
    }

    private func infoPanel(_ file: FileRow) -> some View {
        ScrollView(.vertical, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 20) {
                infoSection("Details") {
                    detail("Kind", file.kind.capitalized + " · " + file.extension.uppercased())
                    if let date = file.displayDate { detail("Captured", date.formatted(date: .abbreviated, time: .shortened)) }
                    detail("Size", ByteCountFormatter.string(fromByteCount: file.sizeBytes, countStyle: .file))
                    if let camera = file.cameraModel, !camera.isEmpty { detail("Camera", camera) }
                    if !people.isEmpty { detail("People", people.joined(separator: ", ")) }
                    if let lat = file.locationLat, let lon = file.locationLon { detail("Location", String(format: "%.4f, %.4f", lat, lon)) }
                }
                if let caption = file.vlmDescription, !caption.isEmpty {
                    infoSection("Description") {
                        Text(caption).font(Theme.Font.body(13)).foregroundStyle(Theme.text1)
                            .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
                        if let model = file.vlmModel, let kind = AIModelKind(rawValue: model) {
                            MetaText("Written by \(kind.displayName)", size: 11)
                        }
                        if let proposed = file.vlmProposedName, !proposed.isEmpty {
                            HStack(spacing: 10) {
                                Image(systemName: "wand.and.rays").foregroundStyle(Theme.gold)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("Smart name").font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3)
                                    Text("\(proposed).\(file.extension)").font(Theme.Font.mono(11.5)).foregroundStyle(Theme.gold)
                                        .lineLimit(2).textSelection(.enabled)
                                }
                                Spacer(minLength: 6)
                                Button("Apply") {
                                    let oldPath = file.pathText
                                    if let newURL = store.applyProposedName(file: file),
                                       newURL.path != oldPath {
                                        // Record the single-file rename so the
                                        // Library "Undo last rename" button can revert
                                        // it. Uses the same UserDefaults slot the
                                        // bulk-rename sheet writes to.
                                        let outcome = ReadStore.RenameOutcome(
                                            fileID: file.id,
                                            oldPath: oldPath,
                                            newPath: newURL.path
                                        )
                                        BulkRenameSheet.saveLastBatch([outcome])
                                        onEdit()
                                    }
                                    dismiss()
                                }
                                .buttonStyle(.dsSecondary)
                                .help("Renames the file on disk and updates the library row. Undo from the Library header if you change your mind.")
                            }
                            .padding(10)
                            .background(Theme.gold.opacity(0.06), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                        }
                    }
                }
                if !tags.isEmpty {
                    infoSection("Tags") {
                        FlowLayout(spacing: 6) {
                            ForEach(tags, id: \.self) { tag in
                                Text(tag).font(Theme.Font.body(11.5, .medium)).foregroundStyle(Theme.text2).lineLimit(1)
                                    .padding(.horizontal, 8).frame(height: 22)
                                    .background(Color.white.opacity(0.06), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
                            }
                        }
                    }
                }
                FinderTagsEditor(file: file, store: store, isFocused: $tagFieldFocused, onEdit: onEdit)
                DisclosureGroup {
                    VStack(spacing: 0) {
                        detail("Path", file.pathText)
                        if file.hasText { detail("Text", "Detected (OCR)") }
                        if let phash = file.phash { detail("pHash", String(phash, radix: 16)) }
                        if let aesthetic = file.aesthetic { detail("Aesthetic", String(format: "%.2f", aesthetic)) }
                    }
                    .padding(.top, 6)
                } label: {
                    SectionLabel("Technical")
                }
            }
            .padding(16)
        }
        .background(Theme.bg1.opacity(0.7), ignoresSafeAreaEdges: [])
    }

    private func infoSection<Content: View>(_ title: String, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionLabel(title)
            VStack(alignment: .leading, spacing: 8) { content() }
        }
    }

    private func detail(_ key: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(key).font(Theme.Font.body(12)).foregroundStyle(Theme.text3).frame(width: 70, alignment: .leading)
            Text(value).font(Theme.Font.body(12.5)).foregroundStyle(Theme.text1).lineLimit(3).textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .padding(.vertical, 5)
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
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
    /// Bound to the preview sheet's tag-field focus so it can suppress
    /// arrow-key navigation while the user is typing a tag.
    var isFocused: FocusState<Bool>.Binding
    /// Notify the parent grid that this file's tags changed so its tile
    /// re-reads the Finder-tag dots — needed for refresh during a live scan.
    var onEdit: () -> Void = {}
    @State private var tags: [String] = []
    @State private var draft: String = ""
    @State private var error: String?
    // Serial chain for off-main tag edits. addTags/removeTags are
    // non-atomic read-modify-write on the xattr, so two quick edits must NOT run
    // concurrently (one would clobber the other). Each edit awaits the previous.
    @State private var tagEditChain: Task<Void, Never>?
    // The file currently displayed. `file` is a by-value `let` captured stale in
    // the edit closure; this @State reads live, so a write that finishes after the
    // user arrowed to another file knows not to apply its result to that file.
    @State private var displayedFileID: Int64?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    SectionLabel("Finder tags")
                    Spacer()
                    MetaText("Shown in Finder and Spotlight", size: 11)
                }
                if tags.isEmpty {
                    Text("No Finder tags yet.")
                        .font(Theme.Font.body(12))
                        .foregroundStyle(.secondary)
                } else {
                    FlowLayout(spacing: 6) {
                        ForEach(tags, id: \.self) { tag in
                            tagPill(tag)
                        }
                    }
                }
                HStack(spacing: 6) {
                    TextField("Add tag…", text: $draft)
                        .textFieldStyle(.plain).font(Theme.Font.body(13.5)).padding(.horizontal, 10).frame(height: 30).background(Color.black.opacity(0.3), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
                        .font(Theme.Font.body(12))
                        .focused(isFocused)
                    Button("Add", action: addDraft)
                        .buttonStyle(DSSecondaryButtonStyle(height: 30))
                        .keyboardShortcut(.defaultAction)
                    .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
                }
                if let e = error {
                    Text(e).font(Theme.Font.body(11)).foregroundStyle(Theme.warning)
                }
            }
        
        .task(id: file.id) {
            // Record the now-displayed file so a still-in-flight tag write for the
            // prior file can detect that the user navigated away (see runTagEdit).
            displayedFileID = file.id
            // Read xattr off the main thread — for files on slow / network
            // volumes the read can stall the preview sheet for hundreds of
            // milliseconds while the user is trying to scrub through.
            let url = file.url
            let result = await Task.detached { TagWriter.readTags(at: url) }.value
            // The detached read doesn't inherit `.task` cancellation; if the
            // user arrowed to another file mid-read, a slow stale read on a
            // network volume could land after the newer one and overwrite it.
            // Mirror FileTile: drop it once `.task(id:)` is superseded.
            guard !Task.isCancelled else { return }
            tags = result
            error = nil
        }
    }

    private func addDraft() {
        let trimmed = draft.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        // Clear the field SYNCHRONOUSLY (the value is captured in
        // `trimmed`) so a slow off-main write can't wipe the user's next-typed tag.
        draft = ""
        runTagEdit { try TagWriter.addTags([trimmed], at: $0) }
    }

    private func remove(_ tag: String) {
        runTagEdit { try TagWriter.removeTags([tag], at: $0) }
    }

    /// Run a non-atomic xattr read-modify-write OFF the main
    /// thread (slow/network volumes can stall the sheet) while SERIALIZING edits
    /// through `tagEditChain` so two quick taps can't interleave their RMW and
    /// lose one. Each edit awaits the previous, then does its write on a detached
    /// task, then assigns on the main actor.
    private func runTagEdit(_ write: @escaping @Sendable (URL) throws -> [String]) {
        let url = file.url
        let targetID = file.id
        let previous = tagEditChain
        tagEditChain = Task { @MainActor in
            _ = await previous?.value   // serialize: the prior edit's write has landed
            do {
                let updated = try await Task.detached { try write(url) }.value
                // The write (to `url`) always persists; only apply the result to the
                // editor's @State if it's still showing the file we edited — else the
                // user navigated away and this would clobber the new file's tags.
                if displayedFileID == targetID {
                    tags = updated
                    error = nil
                }
                store.notifyChanged()   // refresh Library tile tag-count
                onEdit()
            } catch {
                if displayedFileID == targetID {
                    self.error = error.localizedDescription
                }
            }
        }
    }

    @ViewBuilder
    private func tagPill(_ tag: String) -> some View {
        HStack(spacing: 4) {
            Image(systemName: "tag.fill")
                .font(.system(size: 9))
            Text(tag).font(Theme.Font.body(12))
            Button {
                remove(tag)
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Remove this tag")
            .accessibilityLabel("Remove this tag")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(Theme.gold.opacity(0.15)))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.gold.opacity(0.3), lineWidth: 1))
        .foregroundStyle(Theme.gold)
    }
}

/// Video preview: poster frame + Play button (hands off to the default
/// app). AVKit's NSViewRepresentable crashes on macOS 26 in SwiftUI's
/// eager sheet-branch metadata init.
private struct VideoPreview: View {
    let url: URL
    var body: some View { InlineMediaPlayer(url: url, isAudio: false) }
}

/// Audio plays in the sheet with the same native transport controls.
private struct AudioPreview: View {
    let url: URL
    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: "waveform.circle.fill")
                .font(.system(size: 88, weight: .light))
                .foregroundStyle(Theme.gold)
            Text(url.lastPathComponent)
                .font(Theme.Font.body(14, .semibold)).foregroundStyle(Theme.text1)
                .lineLimit(1).truncationMode(.middle)
            InlineMediaPlayer(url: url, isAudio: true)
                .frame(maxWidth: 520, maxHeight: 64)
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// Plays a file inside FileID with AVKit: starts paused on the first frame
/// and stops when the sheet moves on.
private struct InlineMediaPlayer: View {
    let url: URL
    let isAudio: Bool
    @State private var player: AVPlayer?
    @State private var failed = false

    var body: some View {
        ZStack {
            if let player {
                NativeVideoPlayer(player: player)
                    .clipShape(RoundedRectangle(cornerRadius: isAudio ? Theme.Radius.m : 0, style: .continuous))
            } else if failed {
                VStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle").font(.system(size: 28)).foregroundStyle(Theme.warning)
                    Text("This file can't be played on this Mac.").font(Theme.Font.body(13)).foregroundStyle(Theme.text2)
                }
            } else {
                ProgressView().controlSize(.small)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task(id: url) {
            player?.pause()
            player = nil
            failed = false
            let asset = AVURLAsset(url: url)
            let playable = (try? await asset.load(.isPlayable)) ?? false
            guard !Task.isCancelled else { return }
            if playable { player = AVPlayer(playerItem: AVPlayerItem(asset: asset)) } else { failed = true }
        }
        .onDisappear { player?.pause() }
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
                        .font(Theme.Font.body(12))
                        .foregroundStyle(.secondary)
                }
            }
        }
        .task(id: url) {
            // Off the main thread — PDFDocument open + page render can
            // cost real time on a big PDF.
            if let data = await Task.detached(priority: .userInitiated) { () -> Data? in
                guard let doc = PDFDocument(url: url),
                      let page = doc.page(at: 0) else { return nil }
                let bounds = page.bounds(for: .mediaBox)
                // Cap rendered size at 1600 px so giant scans don't OOM.
                let scale = min(1600 / max(bounds.width, bounds.height), 2.0)
                let img = page.thumbnail(of: CGSize(
                    width: bounds.width * scale,
                    height: bounds.height * scale
                ), for: .mediaBox)
                return img.tiffRepresentation
            }.value {
                pageImage = NSImage(data: data)
            }
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
                        .font(Theme.Font.body(13.5))
                        .lineLimit(1).truncationMode(.middle)
                    Text("Open in default app for full preview")
                        .font(Theme.Font.body(11))
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

// MARK: - Selected pane

/// Right-hand details for the clicked tile; double-click opens the full preview.
private struct LibrarySelectedPane: View {
    let row: FileRow
    let store: ReadStore
    let engine: EngineClient
    let onOpen: () -> Void
    let onFindSimilar: () -> Void
    let onClose: () -> Void
    @State private var thumbnail: NSImage?
    @State private var tags: [String] = []
    @State private var people: [String] = []

    var body: some View {
        ScrollView(.vertical, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    SectionLabel("Selected")
                    Spacer()
                    Button(action: onClose) {
                        Image(systemName: "xmark").font(.system(size: 10, weight: .semibold)).foregroundStyle(Theme.text3)
                            .frame(width: 22, height: 22)
                    }
                    .buttonStyle(.dsRow)
                    .keyboardShortcut(.cancelAction)
                    .help("Close details")
                }
                ZStack {
                    Color.black.opacity(0.35)
                    if let thumbnail {
                        Image(nsImage: thumbnail).resizable().aspectRatio(contentMode: .fit)
                    } else {
                        Image(systemName: icon).font(.system(size: 28, weight: .light)).foregroundStyle(Theme.text3)
                    }
                }
                .aspectRatio(16.0 / 10.0, contentMode: .fit)
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
                .onTapGesture(count: 2, perform: onOpen)

                VStack(alignment: .leading, spacing: 3) {
                    Text(row.url.lastPathComponent)
                        .font(Theme.Font.body(14, .semibold)).foregroundStyle(Theme.text1)
                        .lineLimit(2).truncationMode(.middle).textSelection(.enabled)
                    Text(row.url.deletingLastPathComponent().path)
                        .font(Theme.Font.mono(11)).foregroundStyle(Theme.text3)
                        .lineLimit(1).truncationMode(.head)
                }

                VStack(spacing: 0) {
                    detail("Kind", row.kind.capitalized + " · " + row.extension.uppercased())
                    if let date = row.displayDate { detail("Captured", date.formatted(date: .abbreviated, time: .shortened)) }
                    detail("Size", ByteCountFormatter.string(fromByteCount: row.sizeBytes, countStyle: .file))
                    if let camera = row.cameraModel, !camera.isEmpty { detail("Camera", camera) }
                    if !people.isEmpty { detail("People", people.joined(separator: ", ")) }
                }

                if let caption = row.vlmDescription, !caption.isEmpty {
                    Text(caption).font(Theme.Font.body(12.5)).foregroundStyle(Theme.text2)
                        .fixedSize(horizontal: false, vertical: true)
                }

                if !tags.isEmpty {
                    FlowLayout(spacing: 6) {
                        ForEach(tags.prefix(12), id: \.self) { tag in
                            Text(tag)
                                .font(Theme.Font.body(11.5, .medium)).foregroundStyle(Theme.text2)
                                .lineLimit(1).truncationMode(.tail)
                                .padding(.horizontal, 8).frame(height: 22)
                                .frame(maxWidth: 260, alignment: .leading).fixedSize()
                                .background(Color.white.opacity(0.06), in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
                                .help(tag)
                        }
                    }
                }

                VStack(spacing: 6) {
                    Button(action: onOpen) { Label("Open preview", systemImage: "eye").frame(maxWidth: .infinity) }
                        .buttonStyle(DSPrimaryButtonStyle(height: 30))
                        .help("Full preview with tags, smart name and Deep Analyze (or double-click the tile)")
                    HStack(spacing: 6) {
                        if row.kind == "image" || row.kind == "video" {
                            Button(action: onFindSimilar) { Text("Similar").lineLimit(1).frame(maxWidth: .infinity) }
                                .buttonStyle(DSSecondaryButtonStyle(height: 28))
                                .help("Find visually similar files")
                        }
                        Button { NSWorkspace.shared.activateFileViewerSelecting([row.url]) } label: { Text("Finder").lineLimit(1).frame(maxWidth: .infinity) }
                            .buttonStyle(DSSecondaryButtonStyle(height: 28))
                            .help("Show in Finder")
                        Button { NSWorkspace.shared.open(row.url) } label: { Text("Open").lineLimit(1).frame(maxWidth: .infinity) }
                            .buttonStyle(DSSecondaryButtonStyle(height: 28))
                            .help("Open in the default app")
                    }
                }
            }
            .padding(16)
        }
        .background(Theme.bg1.opacity(0.35), ignoresSafeAreaEdges: [])
        .task(id: row.id) {
            thumbnail = nil
            let id = row.id
            let loaded = await Task.detached { [store] in (store.tags(forFileID: id), store.personNames(forFileID: id)) }.value
            guard !Task.isCancelled else { return }
            tags = loaded.0
            people = loaded.1
            let image = row.kind == "image"
                ? await ThumbnailService.previewImage(for: row.url, maxPixelSize: 1024)
                : await ThumbnailService.shared.thumbnail(for: row.url, size: 480)
            guard !Task.isCancelled else { return }
            thumbnail = image
        }
    }

    private var icon: String {
        switch row.kind {
        case "video": return "film"
        case "audio": return "waveform"
        case "pdf": return "doc.richtext"
        default: return "doc"
        }
    }

    private func detail(_ key: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(key).font(Theme.Font.body(12)).foregroundStyle(Theme.text3).frame(width: 64, alignment: .leading)
            Text(value).font(Theme.Font.body(12.5)).foregroundStyle(Theme.text1).lineLimit(2).textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .padding(.vertical, 6)
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
    }
}
