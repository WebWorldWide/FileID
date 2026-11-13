// Dual-pane tree view: folders losing files on the left, folders receiving
// them on the right. Each row carries a status letter matching the legend
// (T = some files move out, R = every file moves, + = files arriving).
import SwiftUI

struct TreeDiffView: View {
    let proposals: [RestructureView.Proposal]
    /// Bumped by the parent whenever `proposals` is replaced; the rows are
    /// rebuilt then, not on every body pass.
    let planVersion: Int
    var onTapSource: (String) -> Void = { _ in }
    var onTapDestination: (String) -> Void = { _ in }
    /// Same hover bus the Sankey writes into. Hovering a folder row
    /// here lights the matching node + ribbons in the diagram, and
    /// vice versa.
    var hoverBus: RestructureHoverBus

    enum FilterMode: String, CaseIterable, Identifiable {
        case all
        case tidy
        case reorganize
        var id: String { rawValue }
        var label: String {
            switch self {
            case .all:        return "All"
            case .tidy:       return "Tidied"
            case .reorganize: return "Reorganized"
            }
        }
    }

    @State private var filter: FilterMode = .all
    @State private var sourceRows: [SourceRow] = []
    @State private var destRows: [DestRow] = []

    // MARK: - Data

    private struct SourceRow: Identifiable {
        var id: String { folder }
        let folder: String
        let displayName: String
        let totalFiles: Int
        let movingFiles: Int
        let kind: SourceKind
    }
    private enum SourceKind { case mixed, junk }

    private struct DestRow: Identifiable {
        var id: String { bucket }
        let bucket: String
        let count: Int
    }

    private func rebuildRows() {
        var kindByFolder: [String: SourceKind] = [:]
        var countByFolder: [String: Int] = [:]
        for p in proposals {
            countByFolder[p.sourceFolder, default: 0] += 1
            if p.kind != .dissolved {
                kindByFolder[p.sourceFolder] = .mixed
            } else if kindByFolder[p.sourceFolder] == nil {
                kindByFolder[p.sourceFolder] = .junk
            }
        }
        let wanted: SourceKind? = filter == .all ? nil : (filter == .tidy ? .mixed : .junk)
        sourceRows = countByFolder.compactMap { folder, count -> SourceRow? in
            let kind = kindByFolder[folder] ?? .junk
            if let wanted, kind != wanted { return nil }
            let display = (folder as NSString).lastPathComponent
            return SourceRow(folder: folder, displayName: display.isEmpty ? folder : display,
                             totalFiles: count, movingFiles: count, kind: kind)
        }
        .sorted { a, b in
            if a.kind != b.kind { return a.kind == .mixed }
            return a.displayName.localizedCaseInsensitiveCompare(b.displayName) == .orderedAscending
        }
        var countByBucket: [String: Int] = [:]
        for p in proposals {
            if let wanted, kindByFolder[p.sourceFolder] != wanted { continue }
            countByBucket[p.bucket, default: 0] += 1
        }
        destRows = countByBucket.map { DestRow(bucket: $0.key, count: $0.value) }
            .sorted { $0.bucket.localizedCaseInsensitiveCompare($1.bucket) == .orderedAscending }
    }

    // MARK: - Body

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                LabeledSegment("Show") {
                    SegmentedBar(selection: $filter, options: FilterMode.allCases.map { ($0, $0.label) })
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
                paneCard(title: "After",
                         subtitle: "Where they'll live",
                         rows: destRows) { row in
                    destRowView(row)
                }
            }
        }
        .onChange(of: planVersion, initial: true) { _, _ in rebuildRows() }
        .onChange(of: filter) { _, _ in rebuildRows() }
    }

    @ViewBuilder
    private var legendStrip: some View {
        HStack(spacing: 10) {
            legendDot(letter: "T", tint: Theme.warning, label: "Tidied: some files move out")
            legendDot(letter: "R", tint: Theme.gold, label: "Reorganized: every file moves")
            legendDot(letter: "+", tint: Theme.delight, label: "Files arrive")
        }
    }

    @ViewBuilder
    private func legendDot(letter: String, tint: Color, label: String) -> some View {
        HStack(spacing: 4) {
            Text(letter)
                .font(Theme.Font.mono(11, .semibold))
                .foregroundStyle(.black)
                .frame(width: 16, height: 16)
                .background(Circle().fill(tint))
            Text(label).font(Theme.Font.body(11)).foregroundStyle(Theme.text2)
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
                Text(title).font(Theme.Font.body(12, .semibold)).foregroundStyle(Theme.text1)
                Text(subtitle).font(Theme.Font.body(11)).foregroundStyle(Theme.text3)
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
        .background(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).fill(.ultraThinMaterial))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)).overlay(RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
        .frame(minHeight: 280, idealHeight: 420, maxHeight: 600)
    }

    // MARK: - Row views

    @ViewBuilder
    private func sourceRowView(_ row: SourceRow) -> some View {
        let (letter, tint) = sourceLetter(for: row.kind)
        let isHighlighted = hoverBus.touchesSource(row.folder)
        let label = sourceRowLabel(row, letter: letter, tint: tint,
                                   isHighlighted: isHighlighted)
        Button { onTapSource(row.folder) } label: { label }
            .buttonStyle(DSRowButtonStyle(radius: Theme.Radius.s))
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
                .font(Theme.Font.mono(11, .semibold))
                .foregroundStyle(.black)
                .frame(width: 18, height: 18)
                .background(Circle().fill(tint))
            Image(systemName: "folder.fill")
                .font(Theme.Font.body(11))
                .foregroundStyle(tint)
            Text(row.displayName)
                .font(Theme.Font.mono(11))
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 6)
            Text("\(row.totalFiles)")
                .font(Theme.Font.body(12).monospacedDigit())
                .foregroundStyle(Theme.text2)
        }
        .padding(.horizontal, 8).padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                .fill(tint.opacity(isHighlighted ? 0.22 : 0.06))
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                .stroke(tint.opacity(isHighlighted ? 0.7 : 0),
                          lineWidth: isHighlighted ? 1 : 0)
        )
        .contentShape(Rectangle())
    }

    @ViewBuilder
    private func destRowView(_ row: DestRow) -> some View {
        let isHighlighted = hoverBus.touchesDest(row.bucket)
        let label = destRowLabel(row, letter: "+", tint: Theme.delight,
                                 isHighlighted: isHighlighted)
        Button { onTapDestination(row.bucket) } label: { label }
            .buttonStyle(DSRowButtonStyle(radius: Theme.Radius.s))
        .help("\(row.count) file\(row.count == 1 ? "" : "s") arriving in this folder.")
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
                .font(Theme.Font.mono(11, .semibold))
                .foregroundStyle(.black)
                .frame(width: 18, height: 18)
                .background(Circle().fill(tint))
            Image(systemName: bucketIconName(row.bucket))
                .font(Theme.Font.body(11))
                .foregroundStyle(tint)
            Text(row.bucket)
                .font(Theme.Font.mono(11))
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 6)
            Text("\(row.count)")
                .font(Theme.Font.body(12).monospacedDigit())
                .foregroundStyle(Theme.text2)
        }
        .padding(.horizontal, 8).padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                .fill(tint.opacity(isHighlighted ? 0.22 : 0.06))
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                .stroke(tint.opacity(isHighlighted ? 0.7 : 0),
                          lineWidth: isHighlighted ? 1 : 0)
        )
        .contentShape(Rectangle())
    }

    private func sourceLetter(for kind: SourceKind) -> (String, Color) {
        switch kind {
        case .mixed:  return ("T", Theme.warning)
        case .junk:   return ("R", Theme.gold)
        }
    }

    private func sourceHelp(for row: SourceRow) -> String {
        switch row.kind {
        case .mixed:
            return "Tidied: \(row.movingFiles) file\(row.movingFiles == 1 ? "" : "s") moving out. The folder stays."
        case .junk:
            return "Reorganized: \(row.totalFiles) file\(row.totalFiles == 1 ? "" : "s") moving out."
        }
    }
}
