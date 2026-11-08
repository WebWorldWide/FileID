import SwiftUI
import AppKit
import FileIDShared

/// Files attached to the next chat question ("12 selected photos").
struct ChatScope: Equatable {
    static let maxFiles = 2000
    var fileIDs: [Int64]
    var label: String

    static func noun(_ kinds: [String], count: Int) -> String {
        let base = kinds.allSatisfy { $0 == "image" } ? "photo" : kinds.allSatisfy { $0 == "video" } ? "video" : "file"
        return count == 1 ? base : base + "s"
    }

    static func selected(_ ids: [Int64], kinds: [String]) -> ChatScope? {
        var seen = Set<Int64>()
        let unique = Array(ids.filter { seen.insert($0).inserted }.prefix(maxFiles))
        guard !unique.isEmpty else { return nil }
        return ChatScope(fileIDs: unique, label: "\(unique.count) selected \(noun(kinds, count: unique.count))")
    }

    /// Chosen answer files, labelled by their kind (photos unless a file plays).
    static func selected(_ ids: [Int64], from hits: [CatalogHit]) -> ChatScope? {
        let paths = Dictionary(hits.map { ($0.fileID, $0.path) }, uniquingKeysWith: { a, _ in a })
        return selected(ids, kinds: ids.map { kind(paths[$0] ?? "") })
    }

    static func kind(_ path: String) -> String {
        MediaPlayback.extensions.contains(URL(fileURLWithPath: path).pathExtension.lowercased()) ? "video" : "image"
    }

    /// The matches of a scoped answer, which become the scope of the next question.
    static func narrowed(from hits: [CatalogHit]) -> ChatScope? {
        let yes = hits.filter { $0.match == "yes" }.map(\.fileID)
        guard !yes.isEmpty else { return nil }
        return ChatScope(fileIDs: yes, label: "\(yes.count) \(yes.count == 1 ? "match" : "matches") from the last answer")
    }
}

extension ChatHandoff {
    /// Opens Chat with these files attached as the next question's scope.
    func askAbout(_ ids: [Int64], kinds: [String]) {
        guard let scope = ChatScope.selected(ids, kinds: kinds) else { return }
        chatScope = scope
        navigate(.chat)
    }
}

struct ChatScopeChip: View {
    let scope: ChatScope
    let onRemove: () -> Void

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "photo.stack").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.gold)
                .accessibilityHidden(true)
            Text(scope.label).font(Theme.Font.body(12, .semibold)).foregroundStyle(Theme.text1).lineLimit(1)
            Button(action: onRemove) {
                Image(systemName: "xmark").font(.system(size: 9, weight: .bold)).foregroundStyle(Theme.text2)
                    .frame(width: 18, height: 18)
            }
            .buttonStyle(DSBareButtonStyle(radius: 9))
            .help("Ask about the whole library again")
            .accessibilityLabel("Remove \(scope.label) from the question")
        }
        .padding(.leading, 9).padding(.trailing, 4)
        .frame(height: 26)
        .background(Theme.gold.opacity(0.12), in: Capsule())
        .overlay(Capsule().stroke(Theme.gold.opacity(0.5), lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Question scope: \(scope.label)")
        .transition(.scale(scale: 0.9).combined(with: .opacity))
    }
}

/// Answers to a scoped question: matches, then the ones the model wasn't
/// sure about. Tiles can be chosen again to narrow the next question, opened,
/// or saved as a Best Takes group.
struct SelectionResultsStrip: View {
    let hits: [CatalogHit]
    @Binding var chosen: Set<Int64>
    var note: String?
    let onOpen: (CatalogHit) -> Void
    let onAsk: ([Int64]) -> Void
    let onGroup: ([Int64]) -> Void

    private var matches: [CatalogHit] { hits.filter { $0.match == "yes" } }
    private var unsure: [CatalogHit] { hits.filter { $0.match == "unsure" } }

    /// The chosen tiles, or every match when none are chosen.
    private var target: [Int64] {
        if !chosen.isEmpty { return hits.map(\.fileID).filter(chosen.contains) }
        return (matches.isEmpty ? hits : matches).map(\.fileID)
    }

    private var askLabel: String {
        if !chosen.isEmpty { return "Ask about \(chosen.count) selected" }
        return matches.isEmpty ? "Ask about these \(hits.count)" : "Ask about the \(matches.count) \(matches.count == 1 ? "match" : "matches")"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !matches.isEmpty { row("Matches · \(matches.count)", matches, color: Theme.gold) }
            if !unsure.isEmpty { row("Not sure · \(unsure.count)", unsure, color: Theme.ai) }
            actions
            if let note { MetaText(note) }
        }
        .padding(.top, 4)
    }

    private func row(_ title: String, _ items: [CatalogHit], color: Color) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            SectionLabel(title, color: color)
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(alignment: .top, spacing: 8) {
                    ForEach(items, id: \.fileID) { hit in
                        SelectionTile(hit: hit, chosen: chosen.contains(hit.fileID), onToggle: { toggle(hit.fileID) }, onOpen: { onOpen(hit) })
                    }
                }
                .padding(3)
            }
        }
    }

    private var actions: some View {
        HStack(spacing: 8) {
            Button(askLabel) { onAsk(target) }
                .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                .help("Ask the next question about only these files")
            Button("Make a group") { onGroup(target) }
                .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                .disabled(target.count < 2)
                .help(target.count < 2 ? "Choose at least two files" : "Save these as a group in Best Takes")
            Button(chosen.count == hits.count ? "Clear" : "Select all") {
                withAnimation(Theme.springStandard) { chosen = chosen.count == hits.count ? [] : Set(hits.map(\.fileID)) }
            }
            .buttonStyle(.dsGhost)
            .accessibilityLabel(chosen.count == hits.count ? "Clear the selection" : "Select every file in this answer")
        }
    }

    private func toggle(_ id: Int64) {
        withAnimation(Theme.springStandard) { if chosen.contains(id) { chosen.remove(id) } else { chosen.insert(id) } }
    }
}

private struct SelectionTile: View {
    let hit: CatalogHit
    let chosen: Bool
    let onToggle: () -> Void
    let onOpen: () -> Void

    private var name: String { URL(fileURLWithPath: hit.path).lastPathComponent }
    private var isMatch: Bool { hit.match == "yes" }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Button(action: onToggle) { thumbnail }
                .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.m))
                .simultaneousGesture(TapGesture(count: 2).onEnded { onOpen() })
            Text(hit.text).font(Theme.Font.body(11.5)).foregroundStyle(Theme.text2).lineLimit(3)
                .frame(width: 132, alignment: .leading)
        }
        .help(name + " · " + hit.text)
        .contextMenu {
            Button("Open") { onOpen() }
            Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: hit.path)]) }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(isMatch ? "Match" : "Not sure"): \(name). \(hit.text)")
        .accessibilityAddTraits(chosen ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction(.default, onToggle)
        .accessibilityAction(named: "Open", onOpen)
    }

    private var thumbnail: some View {
        ZStack(alignment: .topLeading) {
            AsyncThumbnail(url: URL(fileURLWithPath: hit.path), size: 256) { Color.white.opacity(0.04) }
                .frame(width: 132, height: 92)
                .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
            Image(systemName: isMatch ? "checkmark.seal.fill" : "questionmark.circle.fill")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(isMatch ? Theme.gold : Theme.ai)
                .padding(5)
                .background(Color.black.opacity(0.45), in: Circle())
                .padding(4)
        }
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous)
            .stroke(chosen ? Theme.gold : Theme.hairlineStrong, lineWidth: chosen ? 2 : 1))
        .scaleEffect(chosen ? 0.97 : 1)
    }
}
