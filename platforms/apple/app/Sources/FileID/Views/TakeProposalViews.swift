import FileIDShared
import SwiftUI

/// The Best Takes rail section for library-wide similar takes: the find button
/// (the takeGrouping job, with its progress and controls) and the stored
/// proposals. A relation chip marks only the proposals whose relation differs
/// from the section's majority (see `TakeProposals.relationsShowingChip`).
struct RelatedTriesSection: View {
    let proposals: [CatalogTakeAutoGroup]
    let job: CatalogJob?
    let loaded: Bool
    let selectedKey: String?
    let onFind: () -> Void
    let onControl: (String) -> Void
    let onOpen: (CatalogTakeAutoGroup) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            header
            MetaText("Takes and other angles of the same moment, found across your whole library. Suggestions never change your files.", size: 11.5)
                .padding(.horizontal, Theme.Metrics.railInset).padding(.bottom, 6)
                .fixedSize(horizontal: false, vertical: true)
            Button("Find similar takes", action: onFind)
                .buttonStyle(DSAccentOutlineButtonStyle(height: Theme.Metrics.hitTarget))
                .disabled(!TakeProposals.canFind(job))
                .help(TakeProposals.canFind(job) ? "Look through the whole library for takes of the same moment. Runs on this Mac." : "Already looking")
                .padding(.horizontal, Theme.Metrics.railInset).padding(.bottom, 6)
            if let job, job.state != "completed" {
                CatalogJobRow(job: job, control: onControl)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
            if loaded && proposals.isEmpty && job?.state != "running" {
                MetaText(job == nil ? "FileID hasn’t looked yet." : "No similar takes found yet.", size: 12)
                    .padding(.horizontal, Theme.Metrics.railInset).padding(.vertical, 6)
                    .fixedSize(horizontal: false, vertical: true)
            }
            let chipRelations = TakeProposals.relationsShowingChip(among: proposals.map(\.relation))
            ForEach(proposals) { group in
                ProposalRow(group: group, selected: selectedKey == group.groupKey, showsRelation: chipRelations.contains(group.relation)) { onOpen(group) }
                    .id(group.groupKey)
                    .transition(.opacity.combined(with: .move(edge: .top)))
            }
            if let note = TakeProposals.listLimitNote(shown: proposals.count) {
                MetaText(note, size: 11.5)
                    .padding(.horizontal, Theme.Metrics.railInset).padding(.vertical, 6)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .animation(Theme.springStandard, value: proposals.map(\.groupKey))
        .animation(Theme.springStandard, value: job?.state)
    }

    private var header: some View {
        HStack(spacing: 6) {
            SectionLabel("Similar takes · \(proposals.filter { $0.status == "open" }.count)", color: Theme.gold)
            Spacer(minLength: 4)
        }
        .padding(.leading, Theme.Metrics.railInset).padding(.trailing, 8).padding(.top, 10).padding(.bottom, 2)
    }
}

struct RelationChip: View {
    let relation: String?

    private var color: Color {
        switch TakeProposals.relationTone(relation) {
        case .gold: Theme.gold
        case .info: Theme.info
        case .ai: Theme.ai
        }
    }

    var body: some View {
        if let label = TakeProposals.relationLabel(relation) {
            StatusPill(text: label, color: color)
                .help(TakeProposals.relationHelp(relation) ?? "")
                .accessibilityLabel(label)
                .accessibilityHint(TakeProposals.relationHelp(relation) ?? "")
        }
    }
}

private struct ProposalRow: View {
    let group: CatalogTakeAutoGroup
    let selected: Bool
    let showsRelation: Bool
    let action: () -> Void

    var body: some View {
        let noun = "takes"
        let media = group.kind == "videos" ? "Videos" : "Photos"
        RailRow(selected: selected, action: action) {
            HStack(alignment: .top, spacing: 10) {
                ShotFan(shots: group.shots)
                VStack(alignment: .leading, spacing: 3) {
                    let title = Text("\(group.shots.count) \(noun)").font(Theme.Font.body(13, .medium))
                        .foregroundStyle(group.status == "skipped" ? Theme.text3 : Theme.text1)
                    if showsRelation {
                        ViewThatFits(in: .horizontal) {
                            HStack(spacing: 6) { title.fixedSize(); RelationChip(relation: group.relation).fixedSize() }
                            VStack(alignment: .leading, spacing: 3) { title.fixedSize(); RelationChip(relation: group.relation) }
                        }
                    } else {
                        title
                    }
                    let suffix = group.status == "skipped" ? " · Skipped" : ""
                    ViewThatFits(in: .horizontal) {
                        Text("\(media) · " + TakeReview.dateText(group.capturedAt) + suffix).fixedSize()
                        Text("\(media) · " + TakeReview.shortDateText(group.capturedAt) + suffix).fixedSize()
                        VStack(alignment: .leading, spacing: 0) {
                            Text("\(media) · " + TakeReview.dayText(group.capturedAt) + suffix)
                            Text(TakeReview.clockTimeText(group.capturedAt))
                        }
                        .fixedSize(horizontal: false, vertical: true)
                    }
                    .font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3)
                    if showsRelation || selected {
                        Text(TakeProposals.evidenceText(group.evidence))
                            .font(Theme.Font.body(11.5)).foregroundStyle(Theme.text2).lineLimit(3)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .padding(.vertical, 6)
        }
        .accessibilityLabel(TakeProposals.accessibilityLabel(group, date: TakeReview.dateText(group.capturedAt)))
    }
}

/// Up to three thumbnails fanned in a fixed-width slot, so rows line up.
struct ShotFan: View {
    let shots: [CatalogTakeShot]

    var body: some View {
        ZStack(alignment: .leading) {
            ForEach(Array(shots.prefix(3).enumerated()), id: \.element.primaryFileID) { index, shot in
                AsyncThumbnail(url: URL(fileURLWithPath: shot.path), size: 96) { Color.white.opacity(0.05) }
                    .frame(width: 28, height: 28)
                    .clipShape(RoundedRectangle(cornerRadius: 4, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 4, style: .continuous).stroke(Theme.bg1, lineWidth: 1.5))
                    .offset(x: CGFloat(index) * 16)
                    .zIndex(Double(-index))
            }
        }
        .frame(width: 60, height: 28, alignment: .leading)
        .accessibilityHidden(true)
    }
}
