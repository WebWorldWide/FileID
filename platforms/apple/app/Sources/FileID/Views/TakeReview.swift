import AppKit
import AVKit
import FileIDShared
import SwiftUI

/// Review one suggested group of tries: side-by-side compare (best vs. the
/// focused try), a filmstrip with the recommendation starred, plain reasons,
/// keyboard (← → move, Space keep, Z zoom to face, Return keep the best), and
/// one primary action that moves the other tries to the Trash with Undo.
struct TakeReview: View {
    @Bindable var engine: EngineClient
    let group: CatalogTakeAutoGroup
    /// Called after the group is resolved (trashed, kept or skipped) so the
    /// page can refresh its list and open the next group.
    let onResolved: () -> Void

    @State private var qualities: [CatalogTakeQuality] = []
    @State private var loading = true
    @State private var failure: String?
    @State private var focusedIndex = 0
    @State private var keep: Set<Int64> = []
    @State private var zoom = false
    @State private var goal = ""
    @State private var ranking: CatalogTakeGoalRanking?
    @State private var rankingInFlight = false
    @State private var confirmTrash = false
    @State private var trashing = false
    @State private var player: AVPlayer?
    @State private var playingID: Int64?
    @FocusState private var stripFocused: Bool

    private var isVideo: Bool { group.kind == "videos" }
    private var noun: (one: String, many: String) { isVideo ? ("clip", "clips") : ("photo", "photos") }

    private var bestID: Int64? {
        if let ranking, ranking.status == "ranked", let first = ranking.order.first { return first }
        return qualities.first { $0.recommended }?.primaryFileID
    }

    private var victims: [CatalogTakeQuality] { qualities.filter { !keep.contains($0.primaryFileID) } }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header
                if loading {
                    loadingState
                } else if let failure {
                    DashedNotice(title: "Couldn't score these \(noun.many)", subtitle: failure)
                } else if !qualities.isEmpty {
                    compare
                    filmstrip
                    if !isVideo { goalPanel }
                }
            }
            .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, Theme.Metrics.pageInsetY)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: group.groupKey) { await load() }
        .onDisappear { player?.pause() }
        .confirmationDialog(confirmTitle, isPresented: $confirmTrash, titleVisibility: .visible) {
            Button("Move to Trash", role: .destructive) { Task { await trashOthers() } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(confirmMessage)
        }
    }

    // MARK: - Header and actions

    private var header: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text("\(group.shots.count) \(noun.many)").font(Theme.Font.display(28)).foregroundStyle(Theme.text1)
                MetaText(Self.dateText(group.capturedAt), color: Theme.text2, size: 13)
                if group.status == "skipped" {
                    Text("SKIPPED").font(Theme.Font.body(10.5, .semibold)).foregroundStyle(Theme.text3)
                        .padding(.horizontal, 6).padding(.vertical, 2)
                        .overlay(RoundedRectangle(cornerRadius: 4).stroke(Theme.text3.opacity(0.6), lineWidth: 1))
                }
                Spacer(minLength: 0)
            }
            MetaText("Tries of the same shot, taken moments apart. FileID starred the one that looks best. Check it, then keep it and clear out the rest.", size: 13)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Button(primaryTitle) { confirmTrash = true }
                    .buttonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.controlLarge))
                    .disabled(loading || trashing || victims.isEmpty || keep.isEmpty)
                    .keyboardShortcut(.return, modifiers: [])
                    .help(primaryHelp)
                Button("Keep all") { Task { await decide("keepAll") } }
                    .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.control))
                    .disabled(loading || trashing)
                    .help("Keep every \(noun.one) here and stop suggesting this group")
                Button("Skip group") { Task { await decide("skip") } }
                    .buttonStyle(.dsGhost)
                    .disabled(loading || trashing || group.status == "skipped")
                    .help("Decide later. Skipped groups move to the bottom of the list.")
                if trashing { IndeterminateBar().frame(width: 60) }
            }
        }
    }

    private var primaryTitle: String {
        let others = victims.count
        if keep.count <= 1, let best = bestID, keep.contains(best) {
            return "Keep the best, move the other \(others) to Trash"
        }
        return "Keep \(keep.count), move the other \(others) to Trash"
    }

    private var primaryHelp: String {
        if keep.isEmpty { return "Mark at least one \(noun.one) to keep (Space)" }
        if victims.isEmpty { return "Everything here is marked keep. Use Keep all." }
        return "Moves the other \(noun.many) to the Trash. You can undo right after. (Return)"
    }

    private var confirmTitle: String {
        "Move \(victims.count) \(victims.count == 1 ? noun.one : noun.many) to Trash?"
    }

    private var confirmMessage: String {
        let files = victims.reduce(0) { $0 + $1.fileIDs.count }
        let kept = qualities.filter { keep.contains($0.primaryFileID) }.map { URL(fileURLWithPath: $0.path).lastPathComponent }
        let extra = files > victims.count ? " (\(files) files, including RAW copies)" : ""
        return "Keeps \(kept.joined(separator: ", ")). The others\(extra) go to the Trash. You can undo right after, or put them back from the Trash."
    }

    // MARK: - Loading / empty

    private var loadingState: some View {
        VStack(alignment: .leading, spacing: 10) {
            IndeterminateBar().frame(width: 160)
            MetaText("Checking sharpness, faces and exposure in \(group.shots.count) \(noun.many)…", size: 13)
        }
        .padding(.vertical, 24)
        .accessibilityElement(children: .combine)
    }

    // MARK: - Compare

    private var compareIDs: (left: CatalogTakeQuality?, right: CatalogTakeQuality?) {
        let best = qualities.first { $0.primaryFileID == bestID } ?? qualities.first
        let focused = qualities.indices.contains(focusedIndex) ? qualities[focusedIndex] : nil
        if let focused, focused.primaryFileID != best?.primaryFileID { return (best, focused) }
        let runnerUp = qualities.filter { $0.primaryFileID != best?.primaryFileID }.min { $0.rank < $1.rank }
        return (best, runnerUp)
    }

    private var compare: some View {
        let pair = compareIDs
        return HStack(alignment: .top, spacing: 12) {
            if let left = pair.left { comparePane(left, title: "BEST", tone: Theme.gold) }
            if let right = pair.right {
                comparePane(right, title: right.primaryFileID == qualities[safe: focusedIndex]?.primaryFileID ? "THIS TRY" : "RUNNER-UP", tone: Theme.label)
            }
        }
    }

    private func comparePane(_ q: CatalogTakeQuality, title: String, tone: Color) -> some View {
        let index = (qualities.firstIndex { $0.primaryFileID == q.primaryFileID } ?? 0) + 1
        let kept = keep.contains(q.primaryFileID)
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                SectionLabel("\(title) · \(index) of \(qualities.count)", color: tone)
                if q.primaryFileID == bestID {
                    Image(systemName: "star.fill").font(.system(size: 10, weight: .bold)).foregroundStyle(Theme.gold).accessibilityHidden(true)
                }
                Spacer(minLength: 4)
                if kept {
                    Label("Keep", systemImage: "checkmark.circle.fill").font(Theme.Font.body(11.5, .semibold)).foregroundStyle(Theme.delight)
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 9)
            .overlay(alignment: .bottom) { Hairline() }
            ZStack {
                Color.black.opacity(0.35)
                if isVideo, playingID == q.primaryFileID, let player {
                    NativeVideoPlayer(player: player).accessibilityLabel("Video player")
                } else {
                    ZoomedTake(url: URL(fileURLWithPath: q.path), box: zoom ? zoomBox(q) : nil)
                    if isVideo {
                        Button { play(q) } label: {
                            Image(systemName: "play.fill").font(.system(size: 14, weight: .bold)).foregroundStyle(Theme.onGold)
                                .frame(width: 36, height: 36).background(Theme.gold.opacity(0.92), in: Circle())
                        }
                        .buttonStyle(DSBareButtonStyle(radius: 18))
                        .help(q.peakSeconds.map { "Play from the most action, \(Self.clock($0))" } ?? "Play")
                        .accessibilityLabel("Play \(noun.one) \(index)")
                    }
                }
            }
            .frame(height: 340)
            .clipped()
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Text(URL(fileURLWithPath: q.path).lastPathComponent).font(Theme.Font.mono(12, .medium)).foregroundStyle(Theme.text1)
                        .lineLimit(1).truncationMode(.middle).help(q.path)
                    if q.fileIDs.count > 1 { TagChip(text: "+ RAW") }
                    Spacer(minLength: 4)
                    Text("\(Int((q.score * 100).rounded()))").font(Theme.Font.mono(12, .semibold)).foregroundStyle(Theme.text2)
                        .accessibilityLabel("Score \(Int((q.score * 100).rounded())) of 100")
                }
                GrainProgressBar(progress: q.score, tone: q.primaryFileID == bestID ? .gold : .neutral, height: 4)
                    .accessibilityHidden(true)
                ReasonChips(reasons: reasons(for: q), positive: q.primaryFileID == bestID)
            }
            .padding(12)
        }
        .background(Theme.panel, in: RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous)
            .stroke(q.primaryFileID == bestID ? Theme.gold : Theme.hairlineStrong.opacity(0.8), lineWidth: q.primaryFileID == bestID ? 2 : 1))
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(title.capitalized), \(noun.one) \(index) of \(qualities.count), \(URL(fileURLWithPath: q.path).lastPathComponent)")
    }

    private func reasons(for q: CatalogTakeQuality) -> [String] {
        var out = q.reasons
        if let ranking, ranking.status == "ranked" {
            if ranking.metFileIDs.contains(q.primaryFileID) { out.insert("Shows the goal", at: 0) }
            else if q.primaryFileID == ranking.order.first { out.insert("Closest to the goal", at: 0) }
        }
        return Array(out.prefix(4))
    }

    private func zoomBox(_ q: CatalogTakeQuality) -> CatalogTakeBox? {
        let face = q.faces.max { $0.width * $0.height < $1.width * $1.height }
        guard let base = face ?? q.subject else { return nil }
        let factor = face == nil ? 1.2 : 2.6
        let w = min(1, max(0.12, base.width * factor)), h = min(1, max(0.12, base.height * factor))
        let x = min(max(0, base.x + base.width / 2 - w / 2), 1 - w), y = min(max(0, base.y + base.height / 2 - h / 2), 1 - h)
        return CatalogTakeBox(x: x, y: y, width: w, height: h)
    }

    // MARK: - Filmstrip

    private var filmstrip: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                SectionLabel("All \(qualities.count) \(noun.many) · \(keep.count) to keep")
                Spacer(minLength: 8)
                Button(zoom ? "Show whole \(noun.one)" : (isVideo ? "Zoom in" : "Zoom to face")) { withAnimation(Theme.springStandard) { zoom.toggle() } }
                    .buttonStyle(.dsGhost)
                    .help("Z")
                    .accessibilityAddTraits(zoom ? .isSelected : [])
            }
            ScrollViewReader { proxy in
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(Array(qualities.enumerated()), id: \.element.primaryFileID) { index, q in
                            FilmstripTile(quality: q, index: index + 1, count: qualities.count, noun: noun.one,
                                          best: q.primaryFileID == bestID, kept: keep.contains(q.primaryFileID),
                                          focused: index == focusedIndex,
                                          onFocus: { focus(index) }, onToggleKeep: { toggleKeep(q) })
                                .id(q.primaryFileID)
                        }
                    }
                    .padding(4)
                }
                .onChange(of: focusedIndex) { _, index in
                    guard let id = qualities[safe: index]?.primaryFileID else { return }
                    withAnimation(Theme.springStandard) { proxy.scrollTo(id, anchor: .center) }
                }
            }
            .focusable()
            .focused($stripFocused)
            .focusEffectDisabled()
            .overlay {
                if stripFocused {
                    RoundedRectangle(cornerRadius: Theme.Radius.m + 3).stroke(Theme.focusRing, lineWidth: 2).padding(-3).allowsHitTesting(false)
                }
            }
            .onKeyPress(.leftArrow) { focus(focusedIndex - 1); return .handled }
            .onKeyPress(.rightArrow) { focus(focusedIndex + 1); return .handled }
            .onKeyPress(.space) { if let q = qualities[safe: focusedIndex] { toggleKeep(q) }; return .handled }
            .onKeyPress(characters: CharacterSet(charactersIn: "zZ")) { _ in withAnimation(Theme.springStandard) { zoom.toggle() }; return .handled }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("\(qualities.count) \(noun.many). Left and right arrows move, Space marks keep.")
            MetaText("← → move · Space keep · Z zoom · Return keeps the marked \(noun.many) and moves the rest to Trash", size: 11.5)
        }
    }

    // MARK: - Goal

    private var goalPanel: some View {
        Panel {
            PanelHeader("Goal (optional)") {
                if rankingInFlight { IndeterminateBar().frame(width: 60) }
            }
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 8) {
                    DSTextField(placeholder: "What should happen? e.g. Kyle gets a hit, everyone smiling", text: $goal, height: Theme.Metrics.control) {
                        Task { await rankWithGoal() }
                    }
                    Button { Task { await rankWithGoal() } } label: { Label("Rank with AI", systemImage: "sparkles") }
                        .buttonStyle(DSAccentOutlineButtonStyle(color: Theme.ai, height: Theme.Metrics.control))
                        .disabled(goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || rankingInFlight)
                        .help("Looks at all the \(noun.many) together with the description model from Describe Files. Runs on this Mac.")
                }
                if let ranking {
                    HStack(alignment: .top, spacing: 10) {
                        Rectangle().fill(ranking.status == "ranked" ? Theme.ai : Theme.text3).frame(width: 3).accessibilityHidden(true)
                        Text(ranking.summary).font(Theme.Font.body(12.5)).foregroundStyle(Theme.text2)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .accessibilityElement(children: .combine)
                } else {
                    MetaText("Without a goal, FileID picks on sharpness, faces, eyes and exposure. Add a goal to find the try where it happens.", size: 12)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
        }
    }

    // MARK: - Actions

    private func focus(_ index: Int) {
        guard !qualities.isEmpty else { return }
        withAnimation(Theme.spring) { focusedIndex = min(max(0, index), qualities.count - 1) }
        player?.pause(); player = nil; playingID = nil
    }

    private func toggleKeep(_ q: CatalogTakeQuality) {
        withAnimation(Theme.spring) {
            if keep.contains(q.primaryFileID) { keep.remove(q.primaryFileID) } else { keep.insert(q.primaryFileID) }
        }
        let name = URL(fileURLWithPath: q.path).lastPathComponent
        NSAccessibility.post(element: NSApp.mainWindow as Any, notification: .announcementRequested,
                             userInfo: [.announcement: keep.contains(q.primaryFileID) ? "Keeping \(name)" : "Not keeping \(name)"])
    }

    private func play(_ q: CatalogTakeQuality) {
        let url = URL(fileURLWithPath: q.path)
        Task {
            guard let asset = await MediaPlayback.playableAsset(url) else { NSWorkspace.shared.open(url); return }
            player?.pause()
            let next = AVPlayer(playerItem: AVPlayerItem(asset: asset))
            if let peak = q.peakSeconds { await next.seek(to: CMTime(seconds: max(0, peak - 1), preferredTimescale: 600)) }
            withAnimation(Theme.springStandard) { player = next; playingID = q.primaryFileID }
            next.play()
        }
    }

    private func load() async {
        loading = true; failure = nil; ranking = nil; zoom = false; focusedIndex = 0
        player?.pause(); player = nil; playingID = nil
        let response = await TakeRequests.send(engine, CatalogRequest(requestID: UUID().uuidString, action: "scoreTakes", fileIDs: group.fileIDs), timeout: 180)
        guard let response, response.status == "ok", let scored = response.takeQualities, !scored.isEmpty else {
            failure = response?.message ?? "FileID didn't answer in time. Try again in a moment."
            loading = false
            return
        }
        withAnimation(Theme.springStandard) {
            qualities = scored
            keep = Set(scored.filter(\.recommended).map(\.primaryFileID))
            focusedIndex = scored.firstIndex { $0.recommended } ?? 0
            loading = false
        }
        stripFocused = true
    }

    private func rankWithGoal() async {
        let text = goal.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !rankingInFlight else { return }
        rankingInFlight = true
        defer { rankingInFlight = false }
        let order = qualities.sorted { $0.rank < $1.rank }.map(\.primaryFileID)
        let response = await TakeRequests.send(engine, CatalogRequest(requestID: UUID().uuidString, action: "rankTakesWithGoal", query: text, fileIDs: order), timeout: 300)
        guard let response, response.status == "ok", let result = response.takeGoalRanking else {
            ToastCenter.shared.show(response?.message ?? "The goal check didn't finish. The ranking stays on photo quality.")
            return
        }
        withAnimation(Theme.springStandard) {
            ranking = result
            if result.status == "ranked", let best = result.order.first {
                let previous = qualities.first(where: \.recommended)?.primaryFileID
                if keep == Set([previous].compactMap { $0 }) { keep = [best] }
                focusedIndex = qualities.firstIndex { $0.primaryFileID == best } ?? focusedIndex
            }
        }
    }

    private func decide(_ decision: String) async {
        let request = CatalogRequest(requestID: UUID().uuidString, action: "decideTakeGroup",
                                     takeDecision: CatalogTakeDecision(groupKey: group.groupKey, decision: decision, fileIDs: group.fileIDs))
        guard let response = await TakeRequests.send(engine, request, timeout: 30), response.status == "ok" else {
            ToastCenter.shared.show("That didn't save. Try again.")
            return
        }
        let group = self.group, engine = self.engine, onResolved = self.onResolved
        let text = decision == "keepAll" ? "Kept all \(group.shots.count). This group won't be suggested again." : "Skipped. It's at the bottom of the list."
        ToastCenter.shared.show(text, actionLabel: "Undo", duration: 8) {
            Task {
                _ = await TakeRequests.send(engine, CatalogRequest(requestID: UUID().uuidString, action: "decideTakeGroup",
                                                                    takeDecision: CatalogTakeDecision(groupKey: group.groupKey, decision: "reopen", fileIDs: group.fileIDs)), timeout: 30)
                onResolved()
            }
        }
        onResolved()
    }

    private func trashOthers() async {
        guard !trashing else { return }
        let doomed = victims
        let keptShots = qualities.filter { keep.contains($0.primaryFileID) }
        let ids = doomed.flatMap(\.fileIDs)
        guard !ids.isEmpty else { return }
        trashing = true
        defer { trashing = false }
        let signal = engine.bulkActionResultSignal
        guard engine.send(.trashFiles(fileIDs: ids, exactIdentities: nil)) else {
            ToastCenter.shared.show("FileID is still getting ready. Try again in a moment.")
            return
        }
        guard let result = await TakeRequests.bulkResult(engine, after: signal, prefix: "trashFiles", timeout: 120) else {
            ToastCenter.shared.show("Moving to Trash didn't finish. Check the Trash before trying again.")
            onResolved()
            return
        }
        let keptIDs = keptShots.flatMap(\.fileIDs)
        if keptShots.count >= 2 {
            _ = await TakeRequests.send(engine, CatalogRequest(requestID: UUID().uuidString, action: "decideTakeGroup",
                                                                takeDecision: CatalogTakeDecision(groupKey: group.groupKey, decision: "keepAll", fileIDs: keptIDs)), timeout: 30)
        }
        let moved = Set(result.messages.filter(\.ok).compactMap(\.fileID))
        let movedShots = doomed.filter { !moved.isDisjoint(with: $0.fileIDs) }.count
        let batch = result.action.split(separator: ":", maxSplits: 1).dropFirst().first.map(String.init)
        var text = "Moved \(movedShots) \(movedShots == 1 ? noun.one : noun.many) to Trash"
        if result.failed > 0 { text += " · \(result.failed) couldn't be moved" }
        let engine = self.engine, onResolved = self.onResolved, group = self.group, keepAllShots = keptShots.count >= 2
        if let batch {
            ToastCenter.shared.show(text, actionLabel: "Undo", duration: 10) {
                Task {
                    let before = engine.bulkActionResultSignal
                    guard engine.send(.restoreFromTrash(batchID: batch)) else { return }
                    let restored = await TakeRequests.bulkResult(engine, after: before, prefix: "restoreFromTrash", timeout: 120)
                    if keepAllShots {
                        _ = await TakeRequests.send(engine, CatalogRequest(requestID: UUID().uuidString, action: "decideTakeGroup",
                                                                            takeDecision: CatalogTakeDecision(groupKey: group.groupKey, decision: "reopen", fileIDs: keptIDs)), timeout: 30)
                    }
                    let count = restored?.succeeded ?? 0
                    ToastCenter.shared.show(count > 0 ? "Put back \(count) file\(count == 1 ? "" : "s")" : (restored?.messages.first?.message ?? "Nothing was put back. Check the Trash."))
                    onResolved()
                }
            }
        } else {
            ToastCenter.shared.show(text)
        }
        onResolved()
    }

    // MARK: - Formatting

    static func dateText(_ seconds: Double) -> String {
        Date(timeIntervalSince1970: seconds).formatted(.dateTime.weekday(.abbreviated).day().month(.abbreviated).hour().minute())
    }

    static func clock(_ seconds: Double) -> String {
        let s = Int(seconds.rounded())
        return s >= 3600 ? String(format: "%d:%02d:%02d", s / 3600, s / 60 % 60, s % 60) : String(format: "%d:%02d", s / 60, s % 60)
    }
}

// MARK: - Request helpers

/// Catalog and bulk replies awaited by polling the engine's published state;
/// EngineClient's own waiter is private to search.
@MainActor
enum TakeRequests {
    static func send(_ engine: EngineClient, _ request: CatalogRequest, timeout: Double) async -> CatalogResponse? {
        guard engine.send(.catalogRequest(request: request)) else { return nil }
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let response = engine.catalogResponses[request.requestID] { return response }
            try? await Task.sleep(for: .milliseconds(40))
            if Task.isCancelled { return nil }
        }
        return nil
    }

    static func bulkResult(_ engine: EngineClient, after signal: Int, prefix: String, timeout: Double) async -> BulkActionResult? {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if engine.bulkActionResultSignal != signal, let result = engine.bulkActionResult, result.action.hasPrefix(prefix) { return result }
            try? await Task.sleep(for: .milliseconds(50))
        }
        return nil
    }
}

// MARK: - Pieces

private struct ReasonChips: View {
    let reasons: [String]
    let positive: Bool
    var body: some View {
        if reasons.isEmpty {
            MetaText("Nothing stands out", size: 11.5)
        } else {
            HStack(spacing: 6) {
                ForEach(reasons, id: \.self) { reason in
                    let good = Self.positives.contains(reason)
                    Text(reason).font(Theme.Font.body(11.5, .medium))
                        .foregroundStyle(good ? (reason.contains("goal") ? Theme.ai : Theme.gold) : Theme.text2)
                        .padding(.horizontal, 7).padding(.vertical, 2)
                        .background((good ? (reason.contains("goal") ? Theme.ai : Theme.gold) : Color.white).opacity(good ? 0.12 : 0.07),
                                    in: RoundedRectangle(cornerRadius: Theme.Radius.xs))
                        .lineLimit(1)
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(reasons.joined(separator: ", "))
        }
    }

    static let positives: Set<String> = ["Sharpest face", "Sharpest", "Eyes open", "Every face visible", "Best exposure",
                                         "Steadiest", "Most action", "Shows the goal", "Closest to the goal"]
}

private struct FilmstripTile: View {
    let quality: CatalogTakeQuality
    let index: Int
    let count: Int
    let noun: String
    let best: Bool
    let kept: Bool
    let focused: Bool
    let onFocus: () -> Void
    let onToggleKeep: () -> Void

    var body: some View {
        Button(action: onFocus) {
            ZStack(alignment: .topLeading) {
                AsyncThumbnail(url: URL(fileURLWithPath: quality.path), size: 192) { Color.white.opacity(0.04) }
                    .frame(width: 112, height: 76)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
                    .opacity(kept ? 1 : 0.62)
                HStack(spacing: 4) {
                    Text("\(index)").font(Theme.Font.mono(10.5, .semibold)).foregroundStyle(best ? Theme.onGold : Theme.text1)
                        .padding(.horizontal, 5).padding(.vertical, 1)
                        .background(best ? Theme.gold : Color.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 3))
                    if best { Image(systemName: "star.fill").font(.system(size: 9, weight: .bold)).foregroundStyle(Theme.gold) }
                }
                .padding(5)
                if kept {
                    Image(systemName: "checkmark.circle.fill").font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(Theme.onGold, Theme.delight)
                        .padding(5)
                        .frame(width: 112, height: 76, alignment: .topTrailing)
                }
            }
            .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous)
                .stroke(focused ? Theme.text1 : (best ? Theme.gold : Theme.hairlineStrong), lineWidth: focused || best ? 2 : 1))
            .scaleEffect(focused ? 1.04 : 1)
        }
        .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.m))
        .focusable(false)
        .simultaneousGesture(TapGesture(count: 2).onEnded { onToggleKeep() })
        .help(URL(fileURLWithPath: quality.path).lastPathComponent + (quality.reasons.isEmpty ? "" : " · " + quality.reasons.joined(separator: ", ")))
        .accessibilityLabel("\(noun.capitalized) \(index) of \(count), \(URL(fileURLWithPath: quality.path).lastPathComponent)\(best ? ", recommended" : "")\(kept ? ", keep" : ""), score \(Int((quality.score * 100).rounded()))")
        .accessibilityHint(quality.reasons.joined(separator: ", "))
        .accessibilityAddTraits(focused ? .isSelected : [])
        .accessibilityAction(named: kept ? "Don't keep" : "Keep", onToggleKeep)
    }
}

/// A take shown whole, or zoomed onto a normalized box (a face or the sharp
/// subject) with the box centred and filling the pane.
private struct ZoomedTake: View {
    let url: URL
    let box: CatalogTakeBox?
    @State private var image: NSImage?

    var body: some View {
        GeometryReader { geo in
            if let image {
                let iw = image.size.width, ih = image.size.height
                if let box, iw > 0, ih > 0 {
                    let k = min(geo.size.width / (box.width * iw), geo.size.height / (box.height * ih))
                    let w = iw * k, h = ih * k
                    Image(nsImage: image).resizable().interpolation(.high)
                        .frame(width: w, height: h)
                        .offset(x: geo.size.width / 2 - (box.x + box.width / 2) * w, y: geo.size.height / 2 - (box.y + box.height / 2) * h)
                        .frame(width: geo.size.width, height: geo.size.height, alignment: .topLeading)
                } else {
                    Image(nsImage: image).resizable().interpolation(.high).aspectRatio(contentMode: .fit)
                        .frame(width: geo.size.width, height: geo.size.height)
                }
            } else {
                IndeterminateBar().frame(width: 60).frame(width: geo.size.width, height: geo.size.height)
            }
        }
        .clipped()
        .animation(Theme.springStandard, value: box)
        .task(id: url) { image = await ThumbnailService.shared.thumbnail(for: url, size: 1600) }
        .accessibilityHidden(true)
    }
}

extension Array {
    subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil }
}
