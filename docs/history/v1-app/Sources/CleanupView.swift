import SwiftUI
import SwiftData

struct CleanupView: View {
    @ObservedObject var viewModel: AppViewModel
    @Environment(\.modelContext) private var context

    @State private var selectedTab:    CleanupTab = .junk
    @State private var showingUndo     = false
    @State private var trashedManifest: [URL]     = []
    @State private var showingTrashConfirm = false
    @State private var showingDeleteDupesConfirm = false

    // Cached derived data — recomputed only on the @Query count changes or
    // tab change below, NOT on every body eval. Was the dominant tab-switch
    // and scroll cost (4 reductions × 500-row arrays = ~30 ms per body, fired
    // by every hover, picker tap, and SwiftData notification).
    @State private var cachedScreenshots:        [FileRecord] = []
    @State private var cachedActiveFiles:        [FileRecord] = []
    @State private var cachedReclaimableMB:      Double       = 0
    @State private var cachedCategoryBreakdown:  [(label: String, mb: Double, color: Color)] = []
    @State private var cachedDuplicateSummary:   (groupCount: Int, deletable: Int, reclaimMB: Double) = (0, 0, 0)

    // Descriptors live in statics so the Swift type-checker doesn't time out
    // on the compound #Predicate expressions.
    @Query(CleanupView.junkDescriptor)        private var junkFiles: [FileRecord]
    @Query(CleanupView.duplicatesDescriptor)  private var duplicates: [FileRecord]
    @Query(CleanupView.largeDescriptor)       private var largeFiles: [FileRecord]
    @Query(CleanupView.screenshotDescriptor)  private var screenshotCandidates: [FileRecord]

    private static let junkDescriptor: FetchDescriptor<FileRecord> = {
        var d = FetchDescriptor<FileRecord>(
            predicate: #Predicate { $0.junkScore >= 0.45 && $0.isTrashed == false },
            sortBy: [SortDescriptor(\.junkScore, order: .reverse)])
        d.fetchLimit = 500
        return d
    }()

    private static let duplicatesDescriptor: FetchDescriptor<FileRecord> = {
        var d = FetchDescriptor<FileRecord>(
            predicate: #Predicate { $0.duplicateGroupUUID != nil && $0.isTrashed == false })
        d.fetchLimit = 500
        return d
    }()

    private static let largeDescriptor: FetchDescriptor<FileRecord> = {
        var d = FetchDescriptor<FileRecord>(
            predicate: #Predicate { $0.fileSizeMB > 50 && $0.isTrashed == false },
            sortBy: [SortDescriptor(\.fileSizeMB, order: .reverse)])
        d.fetchLimit = 500
        return d
    }()

    // SwiftData can't predicate on [String] arrays; fetch recent non-trashed
    // and filter in-memory. Capped at 500 (screenshots skew recent; long tail
    // isn't worth the tab-switch cost on big libraries).
    private static let screenshotDescriptor: FetchDescriptor<FileRecord> = {
        var d = FetchDescriptor<FileRecord>(
            predicate: #Predicate { $0.isTrashed == false },
            sortBy: [SortDescriptor(\.creationDate, order: .reverse)])
        d.fetchLimit = 500
        return d
    }()

    enum CleanupTab: String, CaseIterable {
        case junk        = "Junk Files"
        case duplicates  = "Duplicates"
        case screenshots = "Screenshots"
        case large       = "Large Files"
    }

    // Recompute every cached derived value in one pass. Called from .onAppear
    // and the @Query count / tab onChange hooks below.
    private func recomputeCaches() {
        let screenshots = screenshotCandidates.filter { $0.aiTags.contains("Screenshot") }
        cachedScreenshots = screenshots

        let active: [FileRecord]
        switch selectedTab {
        case .junk:        active = junkFiles
        case .duplicates:  active = duplicates
        case .screenshots: active = screenshots
        case .large:       active = largeFiles
        }
        cachedActiveFiles = active
        cachedReclaimableMB = active.reduce(0) { $0 + $1.fileSizeMB }

        // Single pass over each category — sum into a tuple instead of
        // `.reduce` four separate times across the body.
        var ssMB = 0.0
        for f in screenshots { ssMB += f.fileSizeMB }
        var dupMB = 0.0
        for f in duplicates  { dupMB += f.fileSizeMB }
        var junkMB = 0.0
        for f in junkFiles   { junkMB += f.fileSizeMB }
        var lgMB = 0.0
        for f in largeFiles  { lgMB += f.fileSizeMB }
        cachedCategoryBreakdown = [
            ("Screenshots", ssMB,  Color.purple),
            ("Duplicates",  dupMB, Color.orange),
            ("Junk",        junkMB, Color.red),
            ("Large Files", lgMB,  Color.cyan)
        ].filter { $0.mb > 0 }

        // Duplicate summary — groups + best-rank reclaim.
        let groups = Dictionary(grouping: duplicates, by: { $0.duplicateGroupUUID ?? UUID() })
            .values.filter { $0.count >= 2 }
        let deletable = groups.reduce(0) { $0 + ($1.count - 1) }
        let reclaim   = groups.reduce(0.0) { acc, grp in
            let sorted = grp.sorted(by: keeperRank)
            return acc + sorted.dropFirst().reduce(0.0) { $0 + $1.fileSizeMB }
        }
        cachedDuplicateSummary = (groups.count, deletable, reclaim)
    }

    // Tie-breaks: quality → size → age → path depth.
    private func keeperRank(_ a: FileRecord, _ b: FileRecord) -> Bool {
        if a.aestheticScore != b.aestheticScore { return a.aestheticScore > b.aestheticScore }
        if a.fileSizeMB     != b.fileSizeMB     { return a.fileSizeMB > b.fileSizeMB }
        if a.creationDate   != b.creationDate   { return a.creationDate < b.creationDate }
        return a.url.pathComponents.count < b.url.pathComponents.count
    }

    @ViewBuilder
    private var headerLeftContent: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: "trash.slash.fill")
                    .font(.largeTitle)
                    .foregroundStyle(Theme.gold)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Cleanup Center").font(.title.bold())
                    Text("\(junkFiles.count + duplicates.count + cachedScreenshots.count) files flagged")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Cleanup Center — \(junkFiles.count + duplicates.count + cachedScreenshots.count) files flagged")

            Picker("Category", selection: $selectedTab) {
                ForEach(CleanupTab.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .help("Switch between junk, duplicates, screenshots, and large-file views")

            actionButtons
        }
    }

    @ViewBuilder
    private var actionButtons: some View {
        HStack(spacing: 10) {
            trashAllButton
            if selectedTab == .duplicates {
                deleteDuplicatesButton
            }
            Text(String(format: "Frees %.1f MB", cachedReclaimableMB))
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private var trashAllButton: some View {
        Button { showingTrashConfirm = true } label: {
            Label("Trash All \(cachedActiveFiles.count)", systemImage: "trash.fill")
                .fontWeight(.semibold)
        }
        .buttonStyle(.borderedProminent).tint(.red)
        .disabled(cachedActiveFiles.isEmpty)
        .accessibilityLabel("Trash all \(cachedActiveFiles.count) files in current category")
        .accessibilityHint("Moves selected files to Trash. You can undo this action.")
        .help("Move every file in this category to the Trash (undoable for 5 s)")
        .confirmationDialog(
            "Move \(cachedActiveFiles.count) files to Trash?",
            isPresented: $showingTrashConfirm,
            titleVisibility: .visible
        ) {
            Button("Move to Trash", role: .destructive) { trashAll() }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text(String(format: "This frees %.1f MB. Files go to the system Trash and can be restored with Undo for 5 seconds.", cachedReclaimableMB))
        }
    }

    private var deleteDuplicatesButton: some View {
        Button { showingDeleteDupesConfirm = true } label: {
            Label("Delete Duplicates (keep 1)", systemImage: "doc.on.doc.fill")
                .fontWeight(.semibold)
        }
        .buttonStyle(.bordered).tint(.orange)
        .disabled(cachedDuplicateSummary.deletable == 0)
        .accessibilityLabel("Delete \(cachedDuplicateSummary.deletable) duplicate files, keeping one copy per group")
        .help("Keeps the sharpest, largest copy of each duplicate group and trashes the others.")
        .confirmationDialog(
            "Delete \(cachedDuplicateSummary.deletable) duplicates from \(cachedDuplicateSummary.groupCount) groups?",
            isPresented: $showingDeleteDupesConfirm,
            titleVisibility: .visible
        ) {
            Button("Delete Duplicates", role: .destructive) { deleteDuplicatesKeepingBest() }
            Button("Cancel", role: .cancel) { }
        } message: {
            Text(String(
                format: "Keeps the sharpest, largest copy of each group. When quality and size match, keeps the file with the earliest on-disk date (more likely to have original photo metadata). Frees %.1f MB. Undo available for 5 seconds.",
                cachedDuplicateSummary.reclaimMB
            ))
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .top, spacing: 24) {
                    headerLeftContent
                    Spacer()
                    if !cachedCategoryBreakdown.isEmpty {
                        VStack(spacing: 8) {
                            CleanupPieChart(segments: cachedCategoryBreakdown)
                                .frame(width: 130, height: 130)
                                .accessibilityLabel("Pie chart breakdown")
                            CleanupPieLegend(segments: cachedCategoryBreakdown)
                        }
                        .padding(.trailing, 8)
                    }
                }
                HStack(alignment: .top, spacing: 24) {
                    headerLeftContent
                    Spacer()
                }
            }
            .padding(20)
            // Flat background — `.ultraThinMaterial` was redrawing the entire
            // header on every body eval, which fired on every hover/scroll.
            .background(Color.white.opacity(0.04))

            if cachedActiveFiles.isEmpty {
                Spacer()
                VStack(spacing: 12) {
                    Image(systemName: "sparkles")
                        .font(.system(size: 56))
                        .foregroundStyle(Theme.gold.opacity(0.5))
                    Text("Nothing here!").font(.headline).foregroundStyle(.secondary)
                    Text("No \(selectedTab.rawValue.lowercased()) detected in your scanned folder.")
                        .font(.caption).foregroundStyle(.tertiary)
                }
                Spacer()
            } else {
                ScrollView {
                    LazyVGrid(
                        columns: [GridItem(.adaptive(minimum: 160, maximum: 190), spacing: 14)],
                        spacing: 14
                    ) {
                        ForEach(cachedActiveFiles, id: \.id) { file in
                            CleanupFileCard(file: file, viewModel: viewModel, navContext: cachedActiveFiles)
                        }
                    }
                    .padding(20)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.black.opacity(0.3))
        .overlay(alignment: .bottom) {
            if showingUndo {
                UndoToast(onUndo: restoreFromTrash)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .padding(.bottom, 20)
            }
        }
        .animation(.spring(response: 0.4), value: showingUndo)
        .onAppear { recomputeCaches() }
        .onChange(of: junkFiles.count)            { _, _ in recomputeCaches() }
        .onChange(of: duplicates.count)           { _, _ in recomputeCaches() }
        .onChange(of: largeFiles.count)           { _, _ in recomputeCaches() }
        .onChange(of: screenshotCandidates.count) { _, _ in recomputeCaches() }
        .onChange(of: selectedTab)                { _, _ in recomputeCaches() }
    }

    // MARK: - Actions

    private func trashAll() {
        let targets = cachedActiveFiles
        let store   = viewModel.dataStore
        Task { @MainActor in
            var moved:        [URL] = []
            var sourceURLs:   [URL] = []
            for file in targets {
                let result = try? await NSWorkspace.shared.recycle([file.url])
                // recycle returns an empty dict on partial failure without throwing.
                if let trashed = result?[file.url] {
                    file.isTrashed = true
                    moved.append(trashed)
                    sourceURLs.append(file.url)
                } else {
                    NSLog("CleanupView.trashAll recycle failed: \(file.url.path)")
                }
            }
            if !moved.isEmpty {
                trashedManifest = moved
                TrashManifest.save(moved)
                withAnimation { showingUndo = true }
                DispatchQueue.main.asyncAfter(deadline: .now() + 5) {
                    withAnimation { showingUndo = false }
                }
                if let store {
                    Task.detached { await store.reconcilePersonSamples(removed: sourceURLs) }
                }
            }
        }
    }

    private func deleteDuplicatesKeepingBest() {
        let groups = Dictionary(grouping: duplicates, by: { $0.duplicateGroupUUID ?? UUID() })
            .values.filter { $0.count >= 2 }
        let targets: [FileRecord] = groups.flatMap { grp in
            Array(grp.sorted(by: keeperRank).dropFirst())
        }
        guard !targets.isEmpty else { return }
        let store = viewModel.dataStore
        Task { @MainActor in
            var moved:      [URL] = []
            var sourceURLs: [URL] = []
            for file in targets {
                let result = try? await NSWorkspace.shared.recycle([file.url])
                if let trashed = result?[file.url] {
                    file.isTrashed = true
                    moved.append(trashed)
                    sourceURLs.append(file.url)
                } else {
                    NSLog("CleanupView.deleteDuplicatesKeepingBest recycle failed: \(file.url.path)")
                }
            }
            if !moved.isEmpty {
                trashedManifest = moved
                TrashManifest.save(moved)
                withAnimation { showingUndo = true }
                DispatchQueue.main.asyncAfter(deadline: .now() + 5) {
                    withAnimation { showingUndo = false }
                }
                if let store {
                    Task.detached { await store.reconcilePersonSamples(removed: sourceURLs) }
                }
            }
        }
    }

    private func restoreFromTrash() {
        guard let manifest = TrashManifest.load() else { return }
        // NSWorkspace has no restore API; move back from ~/.Trash manually.
        for trashedURL in manifest {
            let trashDir = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".Trash")
            let srcInTrash = trashDir.appendingPathComponent(trashedURL.lastPathComponent)
            let original   = trashedManifest.first { $0.lastPathComponent == trashedURL.lastPathComponent } ?? trashedURL
            try? FileManager.default.moveItem(at: srcInTrash, to: original)
        }
        let allTrashed = (try? context.fetch(FetchDescriptor<FileRecord>(predicate: #Predicate { $0.isTrashed == true }))) ?? []
        for file in allTrashed { file.isTrashed = false }
        TrashManifest.delete()
        trashedManifest = []
        withAnimation { showingUndo = false }
    }
}

// MARK: - Trash Manifest

enum TrashManifest {
    private static var url: URL {
        let support = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first
            ?? FileManager.default.homeDirectoryForCurrentUser
        let dir = support.appendingPathComponent("FileID")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("trash_manifest.json")
    }
    static func save(_ urls: [URL]) { try? JSONEncoder().encode(urls).write(to: url) }
    static func load() -> [URL]?   { (try? Data(contentsOf: url)).flatMap { try? JSONDecoder().decode([URL].self, from: $0) } }
    static func delete()           { try? FileManager.default.removeItem(at: url) }
}

// MARK: - Pie Chart

struct CleanupPieChart: View {
    let segments: [(label: String, mb: Double, color: Color)]
    var total: Double { segments.reduce(0) { $0 + $1.mb } }

    var body: some View {
        ZStack {
            Canvas { ctx, size in
                let center = CGPoint(x: size.width / 2, y: size.height / 2)
                let radius = min(size.width, size.height) / 2 - 4
                var startAngle = Angle.degrees(-90)
                for segment in segments {
                    let sweep = Angle.degrees(360 * segment.mb / max(total, 1))
                    let endAngle = startAngle + sweep
                    var path = Path()
                    path.move(to: center)
                    path.addArc(center: center, radius: radius, startAngle: startAngle, endAngle: endAngle, clockwise: false)
                    path.closeSubpath()
                    ctx.fill(path, with: .color(segment.color.opacity(0.85)))
                    ctx.stroke(path, with: .color(.black.opacity(0.3)), lineWidth: 1.5)
                    startAngle = endAngle
                }
                var hole = Path()
                hole.addEllipse(in: CGRect(x: size.width/2 - radius*0.45, y: size.height/2 - radius*0.45,
                                           width: radius*0.9, height: radius*0.9))
                ctx.fill(hole, with: .color(Color(white: 0.07)))
            }
            VStack(spacing: 0) {
                let totalGB = total / 1024
                if totalGB > 1 {
                    Text(String(format: "%.1f GB", totalGB)).font(.system(size: 13, weight: .bold, design: .rounded))
                } else {
                    Text(String(format: "%.0f MB", total)).font(.system(size: 13, weight: .bold, design: .rounded))
                }
                Text("reclaimable").font(.system(size: 8)).foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - Pie Legend

struct CleanupPieLegend: View {
    let segments: [(label: String, mb: Double, color: Color)]

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(segments.indices, id: \.self) { i in
                let seg = segments[i]
                HStack(spacing: 6) {
                    Circle().fill(seg.color.opacity(0.85))
                        .frame(width: 7, height: 7)
                    Text(seg.label)
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 4)
                    Text(formatMB(seg.mb))
                        .font(.system(size: 10, weight: .medium, design: .rounded))
                        .foregroundStyle(.primary)
                        .monospacedDigit()
                }
            }
        }
        .frame(maxWidth: 150, alignment: .leading)
    }

    private func formatMB(_ mb: Double) -> String {
        mb >= 1024 ? String(format: "%.1f GB", mb / 1024) : String(format: "%.0f MB", mb)
    }
}

// MARK: - File Card

struct CleanupFileCard: View {
    let file: FileRecord
    @ObservedObject var viewModel: AppViewModel
    let navContext: [FileRecord]
    @State private var thumbnail: NSImage?
    @State private var isHovered = false

    var body: some View {
        VStack(spacing: 6) {
            ZStack(alignment: .topTrailing) {
                Rectangle()
                    .fill(Color.white.opacity(0.04))
                    .aspectRatio(1, contentMode: .fit)
                    .overlay {
                        if let img = thumbnail {
                            Image(nsImage: img)
                                .resizable()
                                .scaledToFill()
                        } else {
                            ProgressView()
                                .controlSize(.small)
                                .tint(Theme.gold)
                        }
                    }
                    .clipShape(RoundedRectangle(cornerRadius: 10))

                // Hover-only trash button — not always-mounted (was paying
                // Button + symbolRenderingMode cost per card even when idle).
                if isHovered {
                    Button {
                        let url = file.url
                        let store = viewModel.dataStore
                        let target = file
                        Task {
                            let result = try? await NSWorkspace.shared.recycle([url])
                            if let moved = result, moved[url] != nil {
                                await MainActor.run { target.isTrashed = true }
                                if let store {
                                    await store.reconcilePersonSamples(removed: [url])
                                }
                            } else {
                                NSLog("CleanupView recycle failed: \(url.path)")
                            }
                        }
                    } label: {
                        Image(systemName: "trash.circle.fill")
                            .font(.system(size: 22))
                            .symbolRenderingMode(.palette)
                            .foregroundStyle(.white, .red)
                    }
                    .buttonStyle(.plain).padding(5)
                    .contentShape(Rectangle())
                    .accessibilityLabel("Trash \(file.filename)")
                    .help("Move \(file.filename) to the Trash")
                }

                if let badge = CleanupReasonBadge.of(file) {
                    Image(systemName: badge.symbol)
                        .font(.system(size: 9, weight: .bold))
                        .foregroundStyle(.white)
                        .padding(4)
                        .background(Circle().fill(badge.color))
                        // 7 (was 5) — gives the trash button on hover a bit
                        // more room before it overlaps this badge.
                        .padding(7)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                        .help(cleanupBadgeHelp(file: file, badge: badge))
                        .accessibilityLabel(cleanupBadgeHelp(file: file, badge: badge))
                }
            }

            VStack(alignment: .leading, spacing: 2) {
                Text(file.filename)
                    .font(.system(size: 10, weight: .medium)).lineLimit(1).truncationMode(.middle)

                if !file.junkReasons.isEmpty {
                    Text(file.junkReasons.first ?? "")
                        .font(.system(size: 9)).foregroundStyle(.orange).lineLimit(1)
                }

                HStack {
                    Text(String(format: "%.1f MB", file.fileSizeMB))
                        .font(.system(size: 9)).foregroundStyle(.secondary)
                    Spacer()
                    Text(file.creationDate.formatted(date: .numeric, time: .omitted))
                        .font(.system(size: 9)).foregroundStyle(.tertiary)
                        .help("File creation date on disk. For re-imported photos this may differ from the original photo-capture date.")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(6)
        // Flat background — was `.ultraThinMaterial` + shadow per card,
        // ~30 cards visible × material redraw on every body eval was the
        // dominant tab-mount cost.
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color.white.opacity(0.03))
                .stroke(isHovered ? Color.red.opacity(0.4) : Color.clear, lineWidth: 1)
        )
        .contentShape(Rectangle())
        .onHover { isHovered = $0 }
        .task(id: file.url) { if thumbnail == nil { thumbnail = await ThumbnailService.shared.getThumbnail(for: file.url) } }
        .onTapGesture(count: 2) {
            viewModel.openPreview(file, in: navContext)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(file.filename), \(String(format: "%.1f", file.fileSizeMB)) MB")
        .help(cardHelpText)
    }

    private var cardHelpText: String {
        if !file.junkReasons.isEmpty {
            return "\(file.filename)\n" + file.junkReasons.joined(separator: "\n")
        }
        return "\(file.filename) — double-click to open preview"
    }
}

// MARK: - Cleanup badge model

struct CleanupReasonBadge {
    let symbol: String
    let color: Color
    let kind: Kind

    enum Kind { case duplicate, lowAesthetic, empty, cache, tagged, unreadable, large, screenshot }

    static func of(_ file: FileRecord) -> CleanupReasonBadge? {
        let primary = file.junkReasons.first ?? ""
        if primary.contains("Duplicate") || file.duplicateGroupUUID != nil {
            return .init(symbol: "doc.on.doc.fill", color: .orange, kind: .duplicate)
        }
        if primary.contains("aesthetic") {
            return .init(symbol: "photo.badge.exclamationmark.fill", color: .orange, kind: .lowAesthetic)
        }
        if primary.contains("Empty") || primary.contains("unreadable") {
            return .init(symbol: "doc.badge.minus.fill", color: .red, kind: .empty)
        }
        if primary.contains("cache") || primary.contains("temp") {
            return .init(symbol: "trash.slash.fill", color: .red, kind: .cache)
        }
        if primary.contains("Tagged") {
            return .init(symbol: "tag.slash.fill", color: .orange, kind: .tagged)
        }
        if file.aiTags.contains("Screenshot") {
            return .init(symbol: "rectangle.dashed", color: .purple, kind: .screenshot)
        }
        if file.fileSizeMB > 50 {
            return .init(symbol: "arrow.up.doc.fill", color: .cyan, kind: .large)
        }
        return nil
    }
}

private func cleanupBadgeHelp(file: FileRecord, badge: CleanupReasonBadge) -> String {
    if !file.junkReasons.isEmpty {
        return file.junkReasons.joined(separator: "\n")
    }
    switch badge.kind {
    case .duplicate:    return "Duplicate — same content already exists in the library"
    case .lowAesthetic: return "Low aesthetic score — probably blurry or poorly framed"
    case .empty:        return "Empty or unreadable file"
    case .cache:        return "Located in a system cache / temp folder"
    case .tagged:       return "Tagged as junk (cache / temp / screenshot)"
    case .unreadable:   return "Unreadable file"
    case .large:        return String(format: "Large file — %.1f MB", file.fileSizeMB)
    case .screenshot:   return "Screenshot detected"
    }
}

// MARK: - Cleanup file kind

struct CleanupFileKind {
    let symbol: String
    let helpText: String

    static func of(_ url: URL) -> CleanupFileKind {
        let ext = url.pathExtension.lowercased()
        if FileTypes.images.contains(ext) {
            return .init(symbol: "photo.fill", helpText: "Image file")
        }
        if FileTypes.videos.contains(ext) {
            return .init(symbol: "play.rectangle.fill", helpText: "Video file")
        }
        if FileTypes.pdfs.contains(ext) {
            return .init(symbol: "doc.richtext.fill", helpText: "PDF document")
        }
        if FileTypes.word.contains(ext) {
            return .init(symbol: "doc.text.fill", helpText: "Word document")
        }
        if FileTypes.spreadsheet.contains(ext) || ext == "csv" || ext == "tsv" {
            return .init(symbol: "tablecells.fill", helpText: "Spreadsheet")
        }
        if FileTypes.presentation.contains(ext) {
            return .init(symbol: "rectangle.on.rectangle.fill", helpText: "Presentation")
        }
        if FileTypes.richText.contains(ext) {
            return .init(symbol: "doc.richtext", helpText: "Rich text document")
        }
        if FileTypes.plainText.contains(ext) {
            return .init(symbol: "doc.plaintext.fill", helpText: "Text document")
        }
        switch ext {
        case "mp3","wav","m4a","aac","flac":
            return .init(symbol: "waveform", helpText: "Audio file")
        default:
            return .init(symbol: "doc.fill", helpText: "File")
        }
    }
}

// MARK: - Undo Toast

struct UndoToast: View {
    let onUndo: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "trash.fill").foregroundStyle(.red)
            Text("Files moved to Trash").font(.system(size: 13, weight: .medium))
            Spacer()
            Button("Undo", action: onUndo)
                .buttonStyle(.bordered).controlSize(.small)
                .accessibilityLabel("Undo trash action and restore files")
                .help("Restore the files that were just moved to the Trash")
        }
        .padding(.horizontal, 16).padding(.vertical, 10)
        .background(.ultraThickMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .shadow(color: .black.opacity(0.3), radius: 12, y: 4)
        .padding(.horizontal, 40)
    }
}
