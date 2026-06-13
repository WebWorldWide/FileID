// Beyond-Compare-style dual-pane tree view: current state on the
// left, proposed state on the right. Each row carries a git-style
// status letter (M=moved, =unchanged, +=new folder). Filter chips
// jump to moves / new folders / everything.
import SwiftUI

struct TreeDiffView: View {
    let proposals: [RestructureView.Proposal]
    let summary: RestructureView.AssistantSummary
    var onTapSource: (String) -> Void = { _ in }
    var onTapDestination: (String) -> Void = { _ in }
    /// Same hover bus the Sankey writes into. Hovering a folder row
    /// here lights the matching node + ribbons in the diagram, and
    /// vice versa.
    var hoverBus: RestructureHoverBus

    enum FilterMode: String, CaseIterable, Identifiable {
        case all
        case moves
        case newFolders
        var id: String { rawValue }
        var label: String {
            switch self {
            case .all:        return "All"
            case .moves:      return "Moves"
            case .newFolders: return "New folders"
            }
        }
    }

    @State private var filter: FilterMode = .all
    @State private var expandedSources: Set<String> = []
    @State private var expandedDests: Set<String> = []

    // MARK: - Data

    private struct SourceRow: Identifiable {
        var id: String { folder }
        let folder: String
        let displayName: String
        let totalFiles: Int
        let movingFiles: Int
        let kind: SourceKind
    }
    private enum SourceKind { case anchor, mixed, junk }

    private struct DestRow: Identifiable {
        var id: String { bucket }
        let bucket: String
        let count: Int
        let isExisting: Bool
    }

    private var sourceRows: [SourceRow] {
        var rows: [SourceRow] = summary.staysPutBreakdown.map { entry in
            SourceRow(folder: entry.folder, displayName: entry.folder,
                      totalFiles: entry.count, movingFiles: 0, kind: .anchor)
        }
        let bySource = Dictionary(grouping: proposals, by: { $0.sourceFolder })
        for (folder, props) in bySource {
            let display = (folder as NSString).lastPathComponent
            let isJunk = props.allSatisfy { $0.kind == .dissolved }
            rows.append(SourceRow(
                folder: folder,
                displayName: display.isEmpty ? folder : display,
                totalFiles: props.count,
                movingFiles: props.count,
                kind: isJunk ? .junk : .mixed
            ))
        }
        return rows
            .filter(filterMatches)
            .sorted { (a, b) in
                let order: [SourceKind: Int] = [.anchor: 0, .mixed: 1, .junk: 2]
                if order[a.kind, default: 0] != order[b.kind, default: 0] {
                    return order[a.kind, default: 0] < order[b.kind, default: 0]
                }
                return a.displayName.localizedCaseInsensitiveCompare(b.displayName) == .orderedAscending
            }
    }

    private var destRows: [DestRow] {
        let byBucket = Dictionary(grouping: proposals, by: { $0.bucket })
        var rows = byBucket.map { (bucket, props) in
            DestRow(bucket: bucket, count: props.count, isExisting: false)
        }
        // Anchor "destinations" — folders staying put.
        rows.append(contentsOf: summary.staysPutBreakdown.map {
            DestRow(bucket: $0.folder, count: $0.count, isExisting: true)
        })
        return rows
            .filter(destFilterMatches)
            .sorted { (a, b) in
                if a.isExisting != b.isExisting { return a.isExisting }
                return a.bucket.localizedCaseInsensitiveCompare(b.bucket) == .orderedAscending
            }
    }

    private func filterMatches(_ row: SourceRow) -> Bool {
        switch filter {
        case .all:        return true
        case .moves:      return row.kind != .anchor
        case .newFolders: return row.kind == .junk
        }
    }
    private func destFilterMatches(_ row: DestRow) -> Bool {
        switch filter {
        case .all:        return true
        case .moves:      return !row.isExisting
        case .newFolders: return !row.isExisting
        }
    }

    // MARK: - Body

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Text("Filter:").font(.caption.bold()).foregroundStyle(.secondary)
                ForEach(FilterMode.allCases) { mode in
                    Button {
                        withAnimation(.easeInOut(duration: 0.15)) {
                            filter = mode
                        }
                    } label: {
                        Text(mode.label)
                            .font(.caption.bold())
                            .padding(.horizontal, 10).padding(.vertical, 4)
                            .background(
                                Capsule().fill(filter == mode
                                                ? Theme.gold
                                                : Color.secondary.opacity(0.10))
                            )
                            .foregroundStyle(filter == mode ? .black : .primary)
                    }
                    .buttonStyle(.plain)
                }
                Spacer()
                legendStrip
            }

            HStack(alignment: .top, spacing: 12) {
                paneCard(title: "Today",
                         subtitle: "Where files live now",
                         rows: sourceRows) { row in
                    sourceRowView(row)
                }
                paneCard(title: "Proposed",
                         subtitle: "Where they'll live",
                         rows: destRows) { row in
                    destRowView(row)
                }
            }
        }
    }

    @ViewBuilder
    private var legendStrip: some View {
        HStack(spacing: 10) {
            legendDot(letter: "=", tint: .green, label: "Unchanged")
            legendDot(letter: "M", tint: .orange, label: "Moved")
            legendDot(letter: "+", tint: Theme.gold, label: "New folder")
        }
    }

    @ViewBuilder
    private func legendDot(letter: String, tint: Color, label: String) -> some View {
        HStack(spacing: 4) {
            Text(letter)
                .font(.caption.bold().monospaced())
                .foregroundStyle(.black)
                .frame(width: 16, height: 16)
                .background(Circle().fill(tint))
            Text(label).font(.caption2).foregroundStyle(.secondary)
        }
    }

    // Rows are passed as DATA + a @ViewBuilder, not a pre-materialized
    // [AnyView]. The old version eagerly built every row into an AnyView on
    // each render, defeating LazyVStack virtualization (and erasing row
    // identity) — fatal at 100k+ proposals. Now ForEach over Identifiable
    // data lets the LazyVStack build only the visible rows.
    @ViewBuilder
    private func paneCard<Row: Identifiable, RowContent: View>(
        title: String, subtitle: String,
        rows: [Row],
        @ViewBuilder row: @escaping (Row) -> RowContent
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.caption.bold()).foregroundStyle(.primary)
                Text(subtitle).font(.caption2).foregroundStyle(.tertiary)
            }
            .padding(.horizontal, 10).padding(.top, 8)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 4) {
                    ForEach(rows) { item in
                        row(item)
                    }
                }
                .padding(8)
            }
        }
        .frame(maxWidth: .infinity)
        .background(RoundedRectangle(cornerRadius: 8).fill(.ultraThinMaterial))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.secondary.opacity(0.20), lineWidth: 1))
        .frame(minHeight: 280, idealHeight: 420, maxHeight: 600)
    }

    // MARK: - Row views

    @ViewBuilder
    private func sourceRowView(_ row: SourceRow) -> some View {
        let (letter, tint) = sourceLetter(for: row.kind)
        let isHighlighted = hoverBus.touchesSource(row.folder)
        let label = sourceRowLabel(row, letter: letter, tint: tint,
                                   isHighlighted: isHighlighted)
        Group {
            // Anchor folders stay put — they generate no proposals, so a
            // drill-down would open an empty "Nothing to show". Render the
            // anchor row as a static (non-tappable) row instead.
            if row.kind == .anchor {
                label
            } else {
                Button { onTapSource(row.folder) } label: { label }
                    .buttonStyle(.plain)
            }
        }
        .help(sourceHelp(for: row))
        .onHover { hovering in
            hoverBus.set(hovering ? .sourceFolder(row.folder) : nil)
        }
        .animation(.easeInOut(duration: 0.18), value: isHighlighted)
    }

    @ViewBuilder
    private func sourceRowLabel(_ row: SourceRow, letter: String,
                                tint: Color, isHighlighted: Bool) -> some View {
        HStack(spacing: 6) {
            Text(letter)
                .font(.caption.bold().monospaced())
                .foregroundStyle(.black)
                .frame(width: 18, height: 18)
                .background(Circle().fill(tint))
            Image(systemName: "folder.fill")
                .font(.caption2)
                .foregroundStyle(tint)
            Text(row.displayName)
                .font(.caption.monospaced())
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 6)
            Text("\(row.totalFiles)")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 8).padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 4)
                .fill(tint.opacity(isHighlighted ? 0.22 : 0.06))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 4)
                .stroke(tint.opacity(isHighlighted ? 0.7 : 0),
                          lineWidth: isHighlighted ? 1 : 0)
        )
        .contentShape(Rectangle())
    }

    @ViewBuilder
    private func destRowView(_ row: DestRow) -> some View {
        let letter = row.isExisting ? "=" : "+"
        let tint: Color = row.isExisting ? .green : Theme.gold
        let isHighlighted = hoverBus.touchesDest(row.bucket)
        let label = destRowLabel(row, letter: letter, tint: tint,
                                 isHighlighted: isHighlighted)
        Group {
            // An existing/anchor destination is a folder staying put — there
            // are no moving files to drill into, so keep it non-tappable.
            if row.isExisting {
                label
            } else {
                Button { onTapDestination(row.bucket) } label: { label }
                    .buttonStyle(.plain)
            }
        }
        .help(row.isExisting
              ? "Existing folder — \(row.count) file\(row.count == 1 ? "" : "s") staying."
              : "New folder — \(row.count) file\(row.count == 1 ? "" : "s") landing here.")
        .onHover { hovering in
            hoverBus.set(hovering ? .destBucket(row.bucket) : nil)
        }
        .animation(.easeInOut(duration: 0.18), value: isHighlighted)
    }

    @ViewBuilder
    private func destRowLabel(_ row: DestRow, letter: String,
                              tint: Color, isHighlighted: Bool) -> some View {
        HStack(spacing: 6) {
            Text(letter)
                .font(.caption.bold().monospaced())
                .foregroundStyle(.black)
                .frame(width: 18, height: 18)
                .background(Circle().fill(tint))
            Image(systemName: bucketIcon(row.bucket))
                .font(.caption2)
                .foregroundStyle(tint)
            Text(row.bucket)
                .font(.caption.monospaced())
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 6)
            Text("\(row.count)")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 8).padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 4)
                .fill(tint.opacity(isHighlighted ? 0.22 : 0.06))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 4)
                .stroke(tint.opacity(isHighlighted ? 0.7 : 0),
                          lineWidth: isHighlighted ? 1 : 0)
        )
        .contentShape(Rectangle())
    }

    private func sourceLetter(for kind: SourceKind) -> (String, Color) {
        switch kind {
        case .anchor: return ("=", .green)
        case .mixed:  return ("M", .orange)
        case .junk:   return ("M", Theme.gold)
        }
    }

    private func sourceHelp(for row: SourceRow) -> String {
        switch row.kind {
        case .anchor:
            return "Stays put — \(row.totalFiles) file\(row.totalFiles == 1 ? "" : "s") unchanged."
        case .mixed:
            return "Tidy — \(row.movingFiles) file\(row.movingFiles == 1 ? "" : "s") moving out."
        case .junk:
            return "Reorganize — all \(row.totalFiles) file\(row.totalFiles == 1 ? "" : "s") moving."
        }
    }

    private func bucketIcon(_ bucket: String) -> String { bucketIconName(bucket) }
}
