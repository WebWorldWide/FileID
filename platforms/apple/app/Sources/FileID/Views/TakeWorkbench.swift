import AppKit
import AVKit
import FileIDShared
import SwiftUI

struct TakeWorkbench: View {
    @Bindable var engine: EngineClient
    let candidates: [CatalogHit]
    var inline = false

    @Environment(\.dismiss) private var dismiss
    @State private var events: [CatalogEvent] = []
    @State private var selectedEvent: CatalogEvent?
    @State private var takes: [CatalogTake] = []
    @State private var suggestedGroups: [CatalogTakeGroupSuggestion] = []
    @State private var recommendation: CatalogTakeRecommendation?
    @State private var title = ""
    @State private var goal = ""
    @State private var clipQuery = ""
    @State private var foundClips: [CatalogHit] = []
    @State private var selectedIDs: Set<Int64> = []
    @State private var selectedTakeID: Int64?
    @State private var lastDeletedEventID: String?
    @State private var player: AVPlayer?
    @State private var pendingID = ""
    @State private var pendingAction = ""
    @State private var message = ""
    @State private var goalJobID: String?
    @State private var goalEventID: String?
    @State private var goalRequestedIDs: Set<Int64> = []
    @State private var previousJobIDs: Set<String> = []

    private var goalJob: CatalogJob? {
        engine.catalogJobs.first { $0.id == goalJobID && selectedEvent?.id == goalEventID }
    }

    private func canAnalyzeGoal(_ event: CatalogEvent) -> Bool {
        pendingID.isEmpty && goal.trimmingCharacters(in: .whitespacesAndNewlines) == event.goal
            && selectedIDs == Set(event.fileIDs) && takes.count == event.fileIDs.count
            && takes.allSatisfy { FileTypes.isVideo(path: $0.path) }
            && !["queued", "running", "paused"].contains(goalJob?.state ?? "")
    }

    private var pool: [CatalogHit] {
        var seen = Set<Int64>()
        return (candidates + foundClips).filter { seen.insert($0.fileID).inserted }
    }

    private var canSave: Bool {
        !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && selectedIDs.count >= 2
    }

    var body: some View {
        VStack(spacing: 0) {
            if !inline {
                HStack {
                    Text("Best Takes").font(Theme.Font.display(24)).foregroundStyle(Theme.text1)
                    Spacer()
                    Button("Done") { dismiss() }.buttonStyle(.dsSecondary).keyboardShortcut(.cancelAction)
                }
                .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, 14)
                .overlay(alignment: .bottom) { Hairline() }
            }
            HStack(spacing: 0) {
                groupsPane
                    .frame(width: 260)
                    .background(Theme.pane, ignoresSafeAreaEdges: [])
                    .focusSection()
                VHairline()
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        groupPanel
                        if let job = goalJob { goalJobRow(job) }
                        if let recommendation { recommendationRow(recommendation) }
                        if !takes.isEmpty { takeGrid }
                        else if selectedEvent == nil {
                            DashedNotice(title: "Pick the best take", subtitle: "1. Search for your clips on the left.  2. Put two or more tries of the same shot in a group and say what should happen.  3. FileID scores each try so you can pick the best.")
                        }
                    }
                    .padding(.horizontal, Theme.Metrics.pageInsetX).padding(.vertical, Theme.Metrics.pageInsetY)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .focusSection()
            }
        }
        .frame(minWidth: inline ? nil : 960, minHeight: inline ? nil : 680)
        .tint(Theme.gold)
        .task { send("listEvents") }
        .onChange(of: engine.catalogResponses) { _, _ in consume() }
        .onChange(of: engine.catalogJobs) { _, _ in refreshGoalJob() }
        .onDisappear { player?.pause() }
    }

    // MARK: - Groups pane

    private var groupsPane: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                DSTextField(placeholder: "Search your videos", text: $clipQuery, icon: "magnifyingglass") {
                    guard !clipQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
                    send("search", query: clipQuery)
                }
                HStack(spacing: 6) {
                    Button("Find related") { send("suggestTakeGroups", fileIDs: pool.map(\.fileID)) }
                        .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                        .disabled(pool.count < 2)
                        .help(pool.count < 2 ? "Search for at least two videos first" : "Suggest groups of clips that look alike and were filmed close together")
                    Button { newGroup() } label: { Label("New group", systemImage: "plus") }
                        .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                }
            }
            .padding(14)
            Hairline()
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    if !suggestedGroups.isEmpty {
                        SectionLabel("Possible groups", color: Theme.ai).padding(.horizontal, 14).padding(.top, 10).padding(.bottom, 4)
                        ForEach(suggestedGroups.indices, id: \.self) { index in
                            SuggestionRow(suggestion: suggestedGroups[index]) {
                                newGroup()
                                foundClips.append(contentsOf: suggestedGroups[index].members.map {
                                    CatalogHit(fileID: $0.fileID, path: $0.path, kind: "video", text: "")
                                })
                                selectedIDs = Set(suggestedGroups[index].members.map(\.fileID))
                                ToastCenter.shared.show("Name the group and describe the outcome, then save")
                            }
                        }
                        MetaText("Based on how the clips look and when they were filmed. Check them before saving.", size: 11.5)
                            .padding(.horizontal, 14).padding(.vertical, 6)
                        Hairline(color: Theme.hairlineSoft).padding(.vertical, 4)
                    }
                    SectionLabel("Groups · \(events.count)").padding(.horizontal, 14).padding(.top, 10).padding(.bottom, 4)
                    ForEach(events, id: \.id) { event in
                        RailRow(selected: selectedEvent?.id == event.id, action: { open(event) }) {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(event.title).font(Theme.Font.body(13, .medium)).foregroundStyle(Theme.text1).lineLimit(1)
                                Text("\(event.fileIDs.count) clips · \(event.goal)").font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3).lineLimit(2)
                            }
                            .padding(.vertical, 7)
                        }
                        .accessibilityLabel("\(event.title), \(event.fileIDs.count) clips, goal: \(event.goal)")
                    }
                    if events.isEmpty {
                        MetaText("No groups yet. Search for videos above, then press New group.").padding(.horizontal, 14).padding(.vertical, 8)
                    }
                }
                .padding(.bottom, 10)
            }
            if let lastDeletedEventID {
                Hairline()
                HStack {
                    MetaText("Group deleted", size: 12)
                    Spacer()
                    Button("Undo") { send("undoEventEdit", eventID: lastDeletedEventID) }
                        .buttonStyle(DSAccentOutlineButtonStyle(height: Theme.Metrics.hitTarget))
                }
                .padding(.horizontal, 14).padding(.vertical, 8)
            }
        }
    }

    // MARK: - Group editor

    private var groupPanel: some View {
        Panel {
            PanelHeader(selectedEvent == nil ? "New group" : "Group") {
                if pendingID.isEmpty == false { IndeterminateBar().frame(width: 60) }
                if let event = selectedEvent {
                    Button("Undo edit") { send("undoEventEdit", eventID: event.id) }.buttonStyle(.dsGhost)
                    Button("Delete") {
                        lastDeletedEventID = event.id
                        send("deleteEvent", eventID: event.id)
                    }
                    .buttonStyle(DSGhostButtonStyle(color: Theme.danger))
                }
            }
            VStack(alignment: .leading, spacing: 10) {
                fieldRow("Name") { DSTextField(placeholder: "Group name, e.g. Alex at bat", text: $title, height: Theme.Metrics.control) }
                fieldRow("Goal") { DSTextField(placeholder: "What should happen, e.g. Alex gets a hit", text: $goal, height: Theme.Metrics.control) }
            }
            .padding(.horizontal, 14).padding(.vertical, 12)
            if !pool.isEmpty {
                Hairline()
                HStack {
                    SectionLabel("Clips · \(selectedIDs.count) of \(pool.count) chosen")
                    Spacer()
                    Button(selectedIDs.count == pool.count ? "Choose none" : "Choose all") {
                        selectedIDs = selectedIDs.count == pool.count ? [] : Set(pool.map(\.fileID))
                    }
                    .buttonStyle(.dsGhost)
                }
                .padding(.horizontal, 14).padding(.top, 6)
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(pool, id: \.fileID) { hit in
                        Toggle(isOn: Binding(
                            get: { selectedIDs.contains(hit.fileID) },
                            set: { on in if on { selectedIDs.insert(hit.fileID) } else { selectedIDs.remove(hit.fileID) } }
                        )) {
                            Text(URL(fileURLWithPath: hit.path).lastPathComponent).font(Theme.Font.mono(12)).foregroundStyle(Theme.text1)
                                .lineLimit(1).truncationMode(.middle)
                        }
                        .toggleStyle(DSCheckboxToggleStyle())
                        .padding(.horizontal, 14)
                        .help(hit.path)
                    }
                }
                .padding(.bottom, 6)
            }
            PanelFooter {
                Button("Save group") { saveGroup() }
                    .buttonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.hitTarget))
                    .disabled(!canSave)
                    .keyboardShortcut("s", modifiers: .command)
                    .help(canSave ? "Save (⌘S)" : "Needs a name, a goal and at least two clips")
                if let event = selectedEvent {
                    Button("Score takes") { analyzeGoal(event) }
                        .buttonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget))
                        .disabled(!canAnalyzeGoal(event))
                        .help("Check each clip against the goal. Uses the description model from Describe Files.")
                    Button("Refresh scores") { load(event.id) }.buttonStyle(.dsGhost)
                }
                Spacer(minLength: 8)
                MetaText(pool.isEmpty ? "Search on the left to add clips." : "Scores are suggestions from a few frames. Your videos are never changed.", size: 11.5)
                    .lineLimit(2)
            }
        }
    }

    private func fieldRow<Field: View>(_ label: String, @ViewBuilder field: () -> Field) -> some View {
        HStack(spacing: 12) {
            Text(label).font(Theme.Font.body(12.5, .medium)).foregroundStyle(Theme.text2)
                .frame(width: 44, alignment: .leading)
                .accessibilityHidden(true)
            field()
        }
    }

    private func goalJobRow(_ job: CatalogJob) -> some View {
        Panel {
            CatalogJobRow(job: job) { action in send(action, jobID: job.id) }
        }
    }

    // MARK: - Recommendation

    private func recommendationRow(_ recommendation: CatalogTakeRecommendation) -> some View {
        let tone: Color
        switch recommendation.status.lowercased() {
        case "ranked", "preferred": tone = Theme.gold
        case "tie": tone = Theme.ai
        default: tone = Theme.text3
        }
        return HStack(alignment: .top, spacing: 12) {
            Rectangle().fill(tone).frame(width: 3).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                SectionLabel("Recommendation · \(recommendation.status.capitalized)", color: tone)
                Text(recommendation.reason).font(Theme.Font.body(13)).foregroundStyle(Theme.text1)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.vertical, 12)
            Spacer(minLength: 0)
        }
        .background(Theme.panel, in: RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous).stroke(Theme.hairlineStrong.opacity(0.8), lineWidth: 1))
        .accessibilityElement(children: .combine)
    }

    // MARK: - Takes

    private var takeGrid: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 260, maximum: 420), spacing: 12)], alignment: .leading, spacing: 12) {
            ForEach(Array(takes.enumerated()), id: \.element.fileID) { index, take in
                TakeCard(
                    index: index + 1,
                    take: take,
                    recommended: recommendation?.fileIDs.contains(take.fileID) == true,
                    playing: selectedTakeID == take.fileID ? player : nil,
                    canReview: selectedEvent != nil,
                    onPlay: { play(take) },
                    onOutcome: { outcome in if let event = selectedEvent { feedback(event.id, take, outcome: outcome) } },
                    onPrefer: {
                        guard let event = selectedEvent else { return }
                        send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: event.id, fileID: take.fileID, outcomeScore: take.outcomeScore, preferred: !take.preferred))
                    },
                    onUndo: { if let event = selectedEvent { send("undoTakeFeedback", fileID: take.fileID, eventID: event.id) } }
                )
            }
        }
    }

    // MARK: - Actions

    private func open(_ event: CatalogEvent) {
        selectedEvent = event
        lastDeletedEventID = nil
        title = event.title
        goal = event.goal
        selectedIDs = Set(event.fileIDs)
        player?.pause()
        player = nil
        selectedTakeID = nil
        load(event.id)
    }

    private func play(_ take: CatalogTake) {
        let url = URL(fileURLWithPath: take.path)
        Task {
            guard let asset = await MediaPlayback.playableAsset(url) else {
                NSWorkspace.shared.open(url)
                return
            }
            player?.pause()
            let next = AVPlayer(playerItem: AVPlayerItem(asset: asset))
            withAnimation(Theme.springStandard) {
                selectedTakeID = take.fileID
                player = next
            }
            next.play()
        }
    }

    private func newGroup() {
        selectedEvent = nil
        takes = []
        recommendation = nil
        title = ""
        goal = ""
        selectedIDs = []
        selectedTakeID = nil
        player?.pause()
        player = nil
    }

    private func saveGroup() {
        guard canSave else { return }
        lastDeletedEventID = nil
        let event = CatalogEvent(id: selectedEvent?.id ?? UUID().uuidString, title: title, goal: goal,
                                 fileIDs: Array(selectedIDs).sorted())
        send("saveEvent", event: event)
    }

    private func analyzeGoal(_ event: CatalogEvent) {
        goalEventID = event.id
        goalRequestedIDs = Set(event.fileIDs)
        previousJobIDs = Set(engine.catalogJobs.map(\.id))
        send("enqueueTimeline", fileIDs: event.fileIDs, timelineMode: "moments")
    }

    private func refreshGoalJob() {
        guard let job = goalJob else { return }
        if job.state == "completed", pendingID.isEmpty, let eventID = goalEventID {
            goalJobID = nil
            load(eventID)
        } else if job.state == "failed" {
            message = job.error ?? "Scoring didn't finish. Scores you already reviewed are kept."
        }
    }

    private func feedback(_ eventID: String, _ take: CatalogTake, outcome: Double?) {
        send("setTakeFeedback", feedback: CatalogTakeFeedback(eventID: eventID, fileID: take.fileID,
                                                            outcomeScore: outcome, preferred: take.preferred))
    }

    private func load(_ eventID: String) { send("takeGroup", eventID: eventID) }

    private func send(_ action: String, query: String? = nil, fileID: Int64? = nil, fileIDs: [Int64]? = nil, event: CatalogEvent? = nil,
                      eventID: String? = nil, feedback: CatalogTakeFeedback? = nil, timelineMode: String? = nil, jobID: String? = nil) {
        let request = CatalogRequest(requestID: UUID().uuidString, action: action, query: query, fileID: fileID,
                                     jobID: jobID, fileIDs: fileIDs, timelineMode: timelineMode, event: event, eventID: eventID, takeFeedback: feedback)
        pendingID = request.requestID
        pendingAction = action
        message = ""
        if !engine.send(.catalogRequest(request: request)) {
            pendingID = ""
            pendingAction = ""
            ToastCenter.shared.show("FileID is still getting ready. Try again in a moment.")
        }
    }

    private func consume() {
        guard !pendingID.isEmpty, let response = engine.catalogResponses[pendingID] else { return }
        let action = pendingAction
        pendingID = ""
        pendingAction = ""
        message = response.message ?? (response.status == "ok" ? "" : "The request failed.")
        if !message.isEmpty { ToastCenter.shared.show(message) }
        guard response.status == "ok" else {
            if action == "deleteEvent" { lastDeletedEventID = nil }
            return
        }
        if action == "search" {
            var seen = Set(candidates.map(\.fileID))
            foundClips = response.hits.filter { FileTypes.isVideo(path: $0.path) && seen.insert($0.fileID).inserted }
            if foundClips.isEmpty { ToastCenter.shared.show("No videos matched “\(clipQuery)”") }
            return
        }
        if action == "suggestTakeGroups" {
            withAnimation(Theme.springStandard) { suggestedGroups = response.suggestedTakeGroups ?? [] }
            if suggestedGroups.isEmpty { ToastCenter.shared.show("No related takes found in these clips") }
        }
        if action == "enqueueTimeline" {
            goalJobID = response.jobs.first { $0.kind == "timelineSample" && Set($0.fileIDs) == goalRequestedIDs && !previousJobIDs.contains($0.id) }?.id
            refreshGoalJob()
        }
        if let events = response.events {
            if ["takeGroup", "saveEvent", "setTakeFeedback", "undoTakeFeedback"].contains(action) {
                if let event = events.first {
                    selectedEvent = event
                    title = event.title
                    goal = event.goal
                    selectedIDs = Set(event.fileIDs)
                }
            } else {
                self.events = events
                if action == "deleteEvent" { newGroup() }
            }
        }
        if let takes = response.takes {
            withAnimation(Theme.springStandard) {
                self.takes = takes
                recommendation = response.recommendation
            }
        }
        if ["saveEvent", "deleteEvent", "undoEventEdit"].contains(action) {
            if action == "undoEventEdit" { lastDeletedEventID = nil }
            send("listEvents")
        }
    }
}

// MARK: - Suggestion row

private struct SuggestionRow: View {
    let suggestion: CatalogTakeGroupSuggestion
    let onReview: () -> Void

    var body: some View {
        let names = suggestion.members.map { URL(fileURLWithPath: $0.path).lastPathComponent }
        VStack(alignment: .leading, spacing: 6) {
            Text(names.joined(separator: ", ")).font(Theme.Font.mono(11.5)).foregroundStyle(Theme.text1).lineLimit(2)
            HStack(spacing: 8) {
                Text("\(names.count) clips · \(Int((suggestion.similarity * 100).rounded()))% alike")
                    .font(Theme.Font.body(11.5)).foregroundStyle(Theme.ai)
                Spacer(minLength: 4)
                Button("Review", action: onReview)
                    .buttonStyle(DSAccentOutlineButtonStyle(color: Theme.ai, height: Theme.Metrics.hitTarget))
                    .help(suggestion.reason)
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 8)
        .background(Theme.ai.opacity(0.08), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.ai.opacity(0.3), lineWidth: 1))
        .padding(.horizontal, 6)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Possible group: \(names.joined(separator: ", "))")
    }
}

// MARK: - Take card

private struct TakeCard: View {
    let index: Int
    let take: CatalogTake
    let recommended: Bool
    let playing: AVPlayer?
    let canReview: Bool
    let onPlay: () -> Void
    let onOutcome: (Double?) -> Void
    let onPrefer: () -> Void
    let onUndo: () -> Void

    private var url: URL { URL(fileURLWithPath: take.path) }
    private var isDraft: Bool { take.modelVersion?.hasPrefix("take-outcome-sequence-v1/") == true }
    private var reviewedByUser: Bool { take.modelVersion == "user" }

    private var outcome: (String, Color) {
        if take.stale { return ("Needs review: the file changed", Theme.warning) }
        guard let score = take.outcomeScore else {
            if reviewedByUser { return ("Unclear", Theme.text2) }
            return (isDraft ? "Not enough evidence" : "Not reviewed", Theme.text3)
        }
        let prefix = isDraft ? "Draft · " : ""
        return score >= 0.5 ? (prefix + "Goal met", Theme.delight) : (prefix + "Goal missed", Theme.danger)
    }

    private var selectedOutcome: Double?? {
        guard reviewedByUser else { return nil }
        guard let score = take.outcomeScore else { return .some(nil) }
        return .some(score >= 0.5 ? 1 : 0)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                SectionLabel("Take \(index)", color: recommended ? Theme.gold : Theme.label)
                if recommended {
                    Image(systemName: "star.fill").font(.system(size: 10, weight: .bold)).foregroundStyle(Theme.gold)
                        .accessibilityLabel("Recommended")
                }
                Spacer(minLength: 4)
                Circle().fill(reviewedByUser ? Theme.gold : (isDraft ? Theme.ai : Theme.text3)).frame(width: 7, height: 7)
                    .help(reviewedByUser ? "Reviewed by you" : (isDraft ? "Suggested by FileID from a few frames" : "No score yet"))
                    .accessibilityLabel(reviewedByUser ? "Reviewed by you" : (isDraft ? "Model draft" : "No score"))
            }
            .padding(.horizontal, 12).padding(.vertical, 9)
            .overlay(alignment: .bottom) { Hairline() }

            ZStack {
                Color.black.opacity(0.35)
                if let playing {
                    NativeVideoPlayer(player: playing).accessibilityLabel("Video player")
                } else {
                    AsyncThumbnail(url: url, size: 480) { EmptyView() }
                    Button(action: onPlay) {
                        Image(systemName: "play.fill").font(.system(size: 14, weight: .bold)).foregroundStyle(Theme.onGold)
                            .frame(width: 36, height: 36).background(Theme.gold.opacity(0.92), in: Circle())
                    }
                    .buttonStyle(DSBareButtonStyle(radius: 18))
                    .help("Play take \(index)")
                    .accessibilityLabel("Play take \(index)")
                }
            }
            .aspectRatio(16 / 9, contentMode: .fit)
            .clipped()

            VStack(alignment: .leading, spacing: 8) {
                Text(url.lastPathComponent).font(Theme.Font.mono(12, .medium)).foregroundStyle(Theme.text1)
                    .lineLimit(1).truncationMode(.middle).help(take.path)
                Text(take.preferred ? "Preferred by you" : outcome.0).font(Theme.Font.body(13, .semibold))
                    .foregroundStyle(take.preferred ? Theme.gold : outcome.1)
                if let why = take.explanation, !why.isEmpty {
                    Text(why).font(Theme.Font.body(12)).foregroundStyle(Theme.text3).lineLimit(3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let quality = take.qualityScore {
                    HStack(spacing: 8) {
                        Text("Sharpness").font(Theme.Font.body(11.5, .medium)).foregroundStyle(Theme.text3).frame(width: 64, alignment: .leading)
                        GrainProgressBar(progress: quality, tone: .neutral, height: 4)
                        Text("\(Int((quality * 100).rounded()))").font(Theme.Font.mono(11)).foregroundStyle(Theme.text3).frame(width: 26, alignment: .trailing)
                    }
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("Sharpness")
                    .accessibilityValue("\(Int((quality * 100).rounded())) percent")
                }
            }
            .padding(12)

            if canReview {
                HStack(spacing: 6) {
                    outcomeButton("Met", value: 1)
                    outcomeButton("Missed", value: 0)
                    outcomeButton("Unclear", value: nil)
                    Spacer(minLength: 4)
                    Button { onPrefer() } label: {
                        Label("Prefer", systemImage: take.preferred ? "star.fill" : "star")
                    }
                    .buttonStyle(DSAccentOutlineButtonStyle(color: take.preferred ? Theme.gold : Theme.text2, height: Theme.Metrics.hitTarget))
                    .accessibilityLabel(take.preferred ? "Preferred" : "Prefer this take")
                    .accessibilityAddTraits(take.preferred ? .isSelected : [])
                    DSIconButton("Undo review", systemImage: "arrow.uturn.backward", tint: Theme.text3, action: onUndo)
                }
                .padding(.horizontal, 10).padding(.vertical, 8)
                .background(Theme.panelHeader, ignoresSafeAreaEdges: [])
                .overlay(alignment: .top) { Hairline() }
            }
        }
        .background(Theme.panel, in: RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous)
            .stroke(recommended ? Theme.gold : Theme.hairlineStrong.opacity(0.8), lineWidth: recommended ? 2 : 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Take \(index), \(url.lastPathComponent)\(recommended ? ", recommended" : "")")
    }

    private func outcomeButton(_ label: String, value: Double?) -> some View {
        let on: Bool = {
            guard let selected = selectedOutcome else { return false }
            return selected == value
        }()
        return Button(label) { onOutcome(value) }
            .buttonStyle(on ? AnyButtonStyle(DSPrimaryButtonStyle(height: Theme.Metrics.hitTarget)) : AnyButtonStyle(DSSecondaryButtonStyle(height: Theme.Metrics.hitTarget)))
            .accessibilityLabel("Goal \(label.lowercased())")
            .accessibilityAddTraits(on ? .isSelected : [])
    }
}
