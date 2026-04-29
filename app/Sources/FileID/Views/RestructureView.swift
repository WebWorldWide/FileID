// Restructure tab: proposed folder hierarchy + diff preview.
// Heuristic priority (first match wins per file):
//   1. Named person → People/<Name>/<Year>/
//   2. GPS          → Places/<bucket>/<Year>/
//   3. OCR / docs   → Documents/
//   4. Year/Month   → Photos/<Year>/<Month>/
//   5. fallback     → Misc/
// VLM proposed_name becomes the new filename when present.
import SwiftUI
import AppKit
import GRDB
import FileIDShared

struct RestructureView: View {
    let store: ReadStore
    let engine: EngineClient

    @State private var libraryRoot: URL?
    @State private var proposals: [Proposal] = []
    @State private var groups: [Group] = []
    @State private var selectedIDs: Set<Int64> = []
    @State private var loading = false
    @State private var status: String?
    @State private var showingPicker = false
    /// Coalesces auto-reloads while Deep Analyze events stream in.
    @State private var lastAutoReloadAt: Date = .distantPast

    struct Proposal: Identifiable, Hashable {
        var id: Int64 { fileID }
        let fileID: Int64
        let oldPath: String
        let newPath: String
        let bucket: String
    }

    struct Group: Identifiable {
        var id: String { bucket }
        let bucket: String
        let proposals: [Proposal]
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header
                if proposals.isEmpty {
                    emptyState
                } else {
                    actionsBar
                    ForEach(groups) { group in
                        groupCard(group)
                    }
                }
                if let s = status {
                    GlassCard {
                        Text(s).font(.callout)
                    }
                }
            }
            .padding(20)
        }
        .fileImporter(isPresented: $showingPicker,
                       allowedContentTypes: [.folder],
                       allowsMultipleSelection: false) { result in
            switch result {
            case .success(let urls):
                if let url = urls.first {
                    libraryRoot = url
                    Task { await regenerate() }
                }
            case .failure: break
            }
        }
        // 3 s throttle for the per-file Deep Analyze stream.
        .onChange(of: engine.deepAnalyzeLast?.fileID ?? -1) { _, _ in
            guard libraryRoot != nil else { return }
            guard Date().timeIntervalSince(lastAutoReloadAt) >= 3.0 else { return }
            lastAutoReloadAt = Date()
            store.notifyChanged()
            Task { await regenerate() }
        }
        .onChange(of: engine.deepAnalyzeComplete?.processed ?? -1) { _, _ in
            guard libraryRoot != nil else { return }
            store.notifyChanged()
            Task { await regenerate() }
        }
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center, spacing: 12) {
                Image(systemName: "rectangle.3.offgrid")
                    .font(.system(size: 32))
                    .foregroundStyle(Theme.gold)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Restructure").font(.title.bold())
                    Text("Preview a proposed folder hierarchy. Nothing moves until you apply selected rows.")
                        .font(.callout).foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    showingPicker = true
                } label: {
                    Label(libraryRoot == nil ? "Pick destination root…" : "Change destination…",
                          systemImage: "folder")
                }
                .buttonStyle(.bordered)
            }
            if let root = libraryRoot {
                Text("Destination: \(root.path)")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1).truncationMode(.head)
            }
            HStack(spacing: 6) {
                Text("Heuristic:").font(.caption.bold())
                Text("People → Places → Documents → Year/Month → Misc")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 14) {
            Image(systemName: "rectangle.3.offgrid")
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(Theme.gold.opacity(0.4))
            if libraryRoot == nil {
                Text("Pick a destination root above")
                    .font(.title3.bold())
                Text("Files won't move yet — you'll see a diff first.")
                    .font(.callout).foregroundStyle(.secondary)
            } else if loading {
                ProgressView("Computing proposals…")
            } else {
                Text("No images to restructure").font(.title3.bold())
                Text("Run a scan first.")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, minHeight: 240)
    }

    private var actionsBar: some View {
        HStack(spacing: 8) {
            Text("\(selectedIDs.count) of \(proposals.count) selected").font(.caption.monospaced())
            Spacer()
            Button("Select all") { selectedIDs = Set(proposals.map(\.fileID)) }
                .buttonStyle(.bordered)
            Button("Clear") { selectedIDs.removeAll() }
                .buttonStyle(.bordered)
            // Default action — symlinks. Originals untouched, the new
            // tree mirrors them via symlinks. Safe and reversible: just
            // delete the symlinks if you don't like the structure.
            Button {
                applySelected(mode: .symlink)
            } label: {
                Label("Apply via symlinks (\(selectedIDs.count))",
                      systemImage: "link")
                    .padding(.horizontal, 12).padding(.vertical, 6)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Theme.gold))
                    .foregroundStyle(.black)
                    .font(.callout.bold())
            }
            .buttonStyle(.plain)
            .disabled(selectedIDs.isEmpty)
            .help("Creates symlinks at the new paths pointing to the originals. Originals stay where they are. Reversible — just delete the symlinks if you don't like the structure.")
            // Once happy, commit the symlinks to real disk moves.
            Button {
                convertAllToRealMoves()
            } label: {
                Label("Convert to real moves",
                      systemImage: "arrow.triangle.swap")
                    .padding(.horizontal, 12).padding(.vertical, 6)
                    .background(RoundedRectangle(cornerRadius: 8)
                        .stroke(Theme.gold.opacity(0.6), lineWidth: 1))
                    .foregroundStyle(Theme.gold)
                    .font(.callout)
            }
            .buttonStyle(.plain)
            .help("Walks every symlink the symlink-apply step created and replaces it with a real on-disk move. Updates the library DB so search + thumbnails track the new paths.")
        }
    }

    private func groupCard(_ g: Group) -> some View {
        let allSelected = g.proposals.allSatisfy { selectedIDs.contains($0.fileID) }
        return GlassCard {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Image(systemName: bucketIcon(g.bucket))
                        .foregroundStyle(Theme.gold)
                    Text(g.bucket).font(.headline)
                    Text("\(g.proposals.count) file\(g.proposals.count == 1 ? "" : "s")")
                        .font(.caption.monospaced()).foregroundStyle(.secondary)
                    Spacer()
                    Button(allSelected ? "Deselect group" : "Select group") {
                        if allSelected {
                            for p in g.proposals { selectedIDs.remove(p.fileID) }
                        } else {
                            for p in g.proposals { selectedIDs.insert(p.fileID) }
                        }
                    }
                    .buttonStyle(.bordered)
                    .font(.caption)
                }
                ForEach(g.proposals.prefix(50)) { p in
                    proposalRow(p)
                }
                if g.proposals.count > 50 {
                    Text("… \(g.proposals.count - 50) more")
                        .font(.caption2).foregroundStyle(.tertiary)
                }
            }
        }
    }

    private func proposalRow(_ p: Proposal) -> some View {
        let on = selectedIDs.contains(p.fileID)
        return HStack(spacing: 8) {
            Image(systemName: on ? "checkmark.square.fill" : "square")
                .foregroundStyle(on ? Theme.gold : .secondary)
                .onTapGesture {
                    if on { selectedIDs.remove(p.fileID) } else { selectedIDs.insert(p.fileID) }
                }
            VStack(alignment: .leading, spacing: 1) {
                Text(URL(fileURLWithPath: p.oldPath).lastPathComponent)
                    .font(.caption.monospaced())
                    .lineLimit(1).truncationMode(.head)
                Text(p.newPath)
                    .font(.caption2.monospaced())
                    .foregroundStyle(Theme.gold.opacity(0.8))
                    .lineLimit(1).truncationMode(.head)
            }
            Spacer()
        }
    }

    private func bucketIcon(_ bucket: String) -> String {
        if bucket.hasPrefix("People")    { return "person.crop.circle.fill" }
        if bucket.hasPrefix("Places")    { return "mappin.circle.fill" }
        if bucket.hasPrefix("Documents") { return "doc.text.fill" }
        if bucket.hasPrefix("Photos")    { return "photo.stack.fill" }
        return "tray.fill"
    }

    // MARK: - Compute

    private func regenerate() async {
        loading = true
        defer { loading = false }
        status = nil
        let root = libraryRoot
        let computed = await Task.detached(priority: .userInitiated) {
            return RestructureEngine.compute(store: store, libraryRoot: root)
        }.value
        proposals = computed
        // Bucketed for display, sorted by bucket size descending.
        let by = Dictionary(grouping: computed, by: { $0.bucket })
        groups = by.map { Group(bucket: $0.key, proposals: $0.value) }
            .sorted { $0.proposals.count > $1.proposals.count }
        selectedIDs = []
    }

    private func applySelected(mode: RestructureEngine.ApplyMode) {
        let toMove = proposals.filter { selectedIDs.contains($0.fileID) }
        Task {
            let result = await RestructureEngine.apply(proposals: toMove,
                                                       store: store, mode: mode)
            let modeLabel = mode == .symlink ? "linked" : "moved"
            status = "\(result.moved) \(modeLabel) · skipped \(result.skipped) · failed \(result.failed)"
                + (result.conflicts.isEmpty ? "" : " · \(result.conflicts.count) conflicts")
            await regenerate()
        }
    }

    private func convertAllToRealMoves() {
        let candidates = proposals
        Task {
            let result = await RestructureEngine.convertSymlinksToMoves(
                proposals: candidates, store: store
            )
            status = "Converted \(result.moved) symlinks to real moves · skipped \(result.skipped) · failed \(result.failed)"
            await regenerate()
        }
    }
}

// MARK: - Engine (app-side)

enum RestructureEngine {

    static func compute(store: ReadStore, libraryRoot: URL?) -> [RestructureView.Proposal] {
        guard let root = libraryRoot else { return [] }
        var all: [FileRow] = []
        let page = 5000
        var offset = 0
        while true {
            let batch = store.files(offset: offset, limit: page, search: "", kindFilter: nil)
            if batch.isEmpty { break }
            all.append(contentsOf: batch)
            offset += batch.count
            if batch.count < page { break }
        }
        let nameMap = Self.fileToPersonNames(store: store)
        let cal = Calendar(identifier: .gregorian)
        var out: [RestructureView.Proposal] = []
        out.reserveCapacity(all.count)
        for f in all {
            let date = f.displayDate
            let year = date.map { String(cal.component(.year, from: $0)) }
            let month = date.map { Self.monthName(cal.component(.month, from: $0)) }
            let bucket: String
            if let names = nameMap[f.id], let first = names.first, !first.isEmpty {
                bucket = "People/\(first)" + (year.map { "/\($0)" } ?? "")
            } else if let lat = f.locationLat, let lon = f.locationLon {
                let latB = (lat * 2).rounded() / 2
                let lonB = (lon * 2).rounded() / 2
                bucket = "Places/" + String(format: "%.1f_%.1f", latB, lonB) + (year.map { "/\($0)" } ?? "")
            } else if f.hasText || f.kind == "pdf" || f.kind == "doc" {
                bucket = "Documents" + (year.map { "/\($0)" } ?? "")
            } else if let y = year {
                bucket = "Photos/\(y)" + (month.map { "/\($0)" } ?? "")
            } else {
                bucket = "Misc"
            }
            let oldURL = f.url
            let ext = oldURL.pathExtension
            let baseName: String
            if let p = f.vlmProposedName, !p.isEmpty {
                baseName = ext.isEmpty ? p : "\(p).\(ext)"
            } else {
                baseName = oldURL.lastPathComponent
            }
            let target = root
                .appendingPathComponent(bucket, isDirectory: true)
                .appendingPathComponent(baseName)
            if target.path != f.pathText {
                out.append(RestructureView.Proposal(
                    fileID: f.id,
                    oldPath: f.pathText,
                    newPath: target.path,
                    bucket: bucket
                ))
            }
        }
        return out
    }

    private static func fileToPersonNames(store: ReadStore) -> [Int64: [String]] {
        var out: [Int64: [String]] = [:]
        for person in store.persons() {
            guard person.hasAnyName, !person.isUnknown else { continue }
            let name = person.displayName
            for f in store.files(forPersonID: person.id, limit: 5000) {
                out[f.id, default: []].append(name)
            }
        }
        return out
    }

    private static func monthName(_ m: Int) -> String {
        let names = ["", "01-Jan","02-Feb","03-Mar","04-Apr","05-May","06-Jun",
                     "07-Jul","08-Aug","09-Sep","10-Oct","11-Nov","12-Dec"]
        return names[max(1, min(12, m))]
    }

    // MARK: Apply

    struct ApplyResult: Sendable {
        let moved: Int
        let skipped: Int
        let failed: Int
        let conflicts: [String]
        let mode: ApplyMode
    }

    enum ApplyMode: Sendable {
        case symlink   // Originals untouched; new tree mirrors via symlinks.
        case realMove  // `mv` on disk; updates DB path_text rows.
    }

    /// Symlink mode (default) leaves originals in place; realMove `mv`s
    /// each file and rewrites its path_text row.
    static func apply(proposals: [RestructureView.Proposal],
                       store: ReadStore, mode: ApplyMode) async -> ApplyResult {
        let fm = FileManager.default
        var moved = 0
        var skipped = 0
        var failed = 0
        var conflicts: [String] = []
        var pathUpdates: [(Int64, String)] = []
        for p in proposals {
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let newURL = URL(fileURLWithPath: p.newPath)
            if oldURL == newURL { skipped += 1; continue }
            do {
                try fm.createDirectory(at: newURL.deletingLastPathComponent(),
                                        withIntermediateDirectories: true)
            } catch {
                failed += 1; continue
            }
            if fm.fileExists(atPath: newURL.path) {
                conflicts.append(p.newPath); skipped += 1; continue
            }
            do {
                switch mode {
                case .symlink:
                    try fm.createSymbolicLink(at: newURL, withDestinationURL: oldURL)
                case .realMove:
                    try fm.moveItem(at: oldURL, to: newURL)
                    pathUpdates.append((p.fileID, newURL.path))
                }
                moved += 1
            } catch {
                failed += 1
            }
        }
        if !pathUpdates.isEmpty {
            await store.updatePathTexts(pathUpdates)
        }
        return ApplyResult(moved: moved, skipped: skipped, failed: failed,
                           conflicts: conflicts, mode: mode)
    }

    /// For each proposal whose newPath is a symlink → original, replace
    /// it with a real move and update the DB.
    static func convertSymlinksToMoves(proposals: [RestructureView.Proposal],
                                        store: ReadStore) async -> ApplyResult {
        let fm = FileManager.default
        var moved = 0
        var skipped = 0
        var failed = 0
        var pathUpdates: [(Int64, String)] = []
        for p in proposals {
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let newURL = URL(fileURLWithPath: p.newPath)
            // Skip if newPath isn't a symlink (hand-edited tree).
            guard let attrs = try? fm.attributesOfItem(atPath: newURL.path),
                  let type = attrs[.type] as? FileAttributeType,
                  type == .typeSymbolicLink else {
                skipped += 1; continue
            }
            do {
                try fm.removeItem(at: newURL)
                try fm.moveItem(at: oldURL, to: newURL)
                moved += 1
                pathUpdates.append((p.fileID, newURL.path))
            } catch {
                failed += 1
            }
        }
        if !pathUpdates.isEmpty {
            await store.updatePathTexts(pathUpdates)
        }
        return ApplyResult(moved: moved, skipped: skipped, failed: failed,
                           conflicts: [], mode: .realMove)
    }
}
