// Runs the actions Chat proposes, only through paths the pages already use:
// engine commands, the Cleanup trash path, People's store edits and the
// suggested-name rename. Anything that changes files or identities waits
// for the confirmation card.
import SwiftUI
import AppKit
import FileIDShared

/// What the rest of the app shares with Chat: the current selection, where
/// to navigate, and queries handed to Library and Search & Moments.
@MainActor
@Observable
final class ChatHandoff {
    static let shared = ChatHandoff()
    var selectedFileIDs: [Int64] = []
    var selectedPersonIDs: [Int64] = []
    var libraryQuery: String?
    var momentsQuery: String?
    var activeTab: MainWindow.Tab = .library
    var libraryRoot: URL?
    @ObservationIgnored var navigate: (MainWindow.Tab) -> Void = { _ in }
    @ObservationIgnored var chooseFolder: (URL) -> Void = { _ in }
    @ObservationIgnored var store: ReadStore?
    @ObservationIgnored var toolsSession: ToolsSession?

    var context: ChatContext {
        let page: String = switch activeTab {
        case .library: "library"
        case .people: "people"
        case .moments, .takes: "search"
        case .deep: "deepAnalyze"
        case .chat: "chat"
        case .cleanup: "cleanup"
        case .restructure: "restructure"
        case .tools: "tools"
        case .settings: "settings"
        }
        return ChatContext(page: page,
                           fileIDs: selectedFileIDs.isEmpty ? nil : Array(selectedFileIDs.prefix(10_000)),
                           personIDs: selectedPersonIDs.isEmpty ? nil : Array(selectedPersonIDs.prefix(50)),
                           folder: libraryRoot?.path, libraryRoot: libraryRoot?.path)
    }

    static func tab(for page: String) -> MainWindow.Tab {
        switch page {
        case "people": .people
        case "cleanup": .cleanup
        case "deepAnalyze": .deep
        case "restructure": .restructure
        case "settings": .settings
        case "search": .moments
        case "tools": .tools
        case "chat": .chat
        default: .library
        }
    }
}

/// Library's side of the hand-off: publish the checked files, take a search.
struct LibraryChatHooks: ViewModifier {
    let checked: [Int64]
    @Binding var searchText: String
    @Binding var similarSeed: FileRow?

    func body(content: Content) -> some View {
        content
            .onChange(of: checked, initial: true) { _, ids in ChatHandoff.shared.selectedFileIDs = ids }
            .onChange(of: ChatHandoff.shared.libraryQuery, initial: true) { _, query in
                guard let query else { return }
                ChatHandoff.shared.libraryQuery = nil
                similarSeed = nil
                searchText = query
            }
    }
}

@MainActor
@Observable
final class ChatActionRunner {
    static let shared = ChatActionRunner()

    enum Phase: Equatable {
        case proposed
        case running(String)
        case done(String)
        case failed(String)
        case cancelled
    }

    private(set) var phases: [String: Phase] = [:]
    private(set) var undoLabels: [String: String] = [:]
    @ObservationIgnored private var undos: [String: () async -> String] = [:]

    func phase(_ action: ChatAction) -> Phase { phases[action.id] ?? .proposed }

    func cancel(_ action: ChatAction) { phases[action.id] = .cancelled }

    /// Live text while a job the action started is running.
    func progress(_ action: ChatAction, engine: EngineClient) -> String? {
        guard case .running(let fallback) = phase(action) else { return nil }
        switch action.kind {
        case "scanFolder", "rescan":
            guard let progress = engine.lastProgress, [.discovering, .tagging, .postScan].contains(progress.phase) else { return fallback }
            if progress.phase == .discovering { return "Finding files · \(progress.discovered.formatted()) so far" }
            if progress.phase == .postScan { return "Finishing up" }
            return "Scanning · \(progress.processed.formatted()) of \(progress.total.formatted()) files"
        case "describeFiles":
            guard let progress = engine.deepAnalyzeProgress else { return engine.deepAnalyzeStarting == nil ? fallback : "Loading the local AI model" }
            return "Describing · \(progress.processed.formatted()) of \(progress.total.formatted()) files"
        default:
            return fallback
        }
    }

    func canStop(_ action: ChatAction) -> Bool {
        guard case .running = phase(action) else { return false }
        return ["scanFolder", "rescan", "describeFiles", "applyFolderLayout", "undoLastMove", "suggestFolderLayout"].contains(action.kind)
    }

    func stop(_ action: ChatAction, engine: EngineClient) {
        switch action.kind {
        case "scanFolder", "rescan": engine.cancelAutoPilot(); engine.cancel()
        case "describeFiles": engine.deepAnalyzeCancel()
        default: engine.cancelRestructure()
        }
    }

    func hasUndo(_ action: ChatAction) -> Bool {
        guard case .done = phase(action) else { return false }
        return undos[action.id] != nil
    }

    func undo(_ action: ChatAction) async {
        guard let undo = undos.removeValue(forKey: action.id) else { return }
        undoLabels[action.id] = nil
        phases[action.id] = .running("Undoing…")
        phases[action.id] = .done(await undo())
    }

    // MARK: - Folder layout plan

    /// The current suggested layout for the action's folder, as the apply
    /// card shows it: what moves, and examples.
    struct PlanSummary {
        let root: String
        let count: Int
        let examples: [String]
        let planID: String?
        let moves: [RestructureMove]
    }

    func planSummary(_ action: ChatAction, engine: EngineClient) -> PlanSummary? {
        guard let plan = engine.restructurePlan, plan.libraryRoot == action.path || action.path == nil else { return nil }
        // A stored plan applies only its Auto tier; a listed plan is narrowed
        // to the same tier so Chat never moves files the plan marked for review.
        let auto = plan.moves.filter { $0.confidence == "auto" }
        let count = plan.planID != nil ? (plan.confidenceCounts?.auto ?? plan.totalMoves ?? plan.moves.count) : auto.count
        guard count > 0 else { return nil }
        let examples = auto.prefix(3).map { move in
            let destination = URL(fileURLWithPath: move.destination).deletingLastPathComponent().path
            let relative = destination.hasPrefix(plan.libraryRoot) ? String(destination.dropFirst(plan.libraryRoot.count)).trimmingCharacters(in: CharacterSet(charactersIn: "/")) : destination
            return "\(URL(fileURLWithPath: move.source).lastPathComponent) → \(relative.isEmpty ? "top folder" : relative)"
        }
        return PlanSummary(root: plan.libraryRoot, count: count, examples: examples, planID: plan.planID, moves: plan.planID == nil ? auto : [])
    }

    // MARK: - Running

    func run(_ action: ChatAction, engine: EngineClient) async {
        guard ChatActions.isRunnable(action) else { phases[action.id] = .failed("This action isn't available."); return }
        if case .running = phase(action) { return }
        let handoff = ChatHandoff.shared
        switch action.kind {
        case "scanFolder", "rescan":
            guard let path = action.path else { return }
            let url = action.kind == "rescan" ? (handoff.libraryRoot ?? URL(fileURLWithPath: path)) : URL(fileURLWithPath: path)
            if action.kind == "scanFolder" { handoff.chooseFolder(url) } else { engine.startScan(rootURL: url) }
            phases[action.id] = .running("Starting the scan")
            await watch(action, started: { [.discovering, .tagging, .postScan].contains(engine.lastProgress?.phase) },
                        finished: { ![.discovering, .tagging, .postScan].contains(engine.lastProgress?.phase) }) {
                guard let progress = engine.lastProgress else { return "Scan finished." }
                if progress.phase == .cancelled { return "Scan stopped. Files found so far stay in the library." }
                return "Scan finished · \(progress.processed.formatted()) files\(progress.failed > 0 ? " · \(progress.failed.formatted()) couldn't be read" : "")."
            }
        case "describeFiles":
            let model = DeepAnalyzeSettings.shared.activeKind.rawValue
            let sent = action.fileIDs.map { engine.deepAnalyzeSelection(fileIDs: Array($0.prefix(100)), modelKind: model) } ?? {
                engine.deepAnalyzeAll(modelKind: model, skipExisting: true)
                return engine.deepAnalyzeInFlight
            }()
            guard sent else { phases[action.id] = .failed("Describing didn't start. Check the AI model in Describe Files."); return }
            phases[action.id] = .running("Starting")
            await watch(action, started: { engine.deepAnalyzeInFlight }, finished: { !engine.deepAnalyzeInFlight }) {
                guard let done = engine.deepAnalyzeComplete else { return "Describing finished." }
                if done.cancelled { return "Stopped after \(done.processed.formatted()) files. Their descriptions are kept." }
                return "Described \(done.processed.formatted()) files\(done.failed > 0 ? " · \(done.failed.formatted()) couldn't be described" : "")."
            }
        case "stopJob":
            let job = action.job ?? "all"
            if job == "scan" || job == "all" { engine.cancelAutoPilot(); engine.cancel() }
            if job == "describe" || job == "all" { engine.deepAnalyzeCancel() }
            if job == "organize" || job == "all" { engine.cancelRestructure() }
            if job == "moments" || job == "all" {
                for running in engine.catalogJobs where running.state == "running" || running.state == "queued" {
                    _ = engine.send(.catalogRequest(request: CatalogRequest(requestID: UUID().uuidString, action: "cancelJob", jobID: running.id)))
                }
            }
            phases[action.id] = .done("Asked FileID to stop. Finished work is kept.")
        case "findDuplicates", "openPage":
            let page = action.page ?? "library"
            handoff.navigate(ChatHandoff.tab(for: page))
            phases[action.id] = .done("Opened \(ChatActions.pageTitle(page)).")
        case "search":
            handoff.libraryQuery = action.query
            handoff.navigate(.library)
            phases[action.id] = .done("Opened Library with this search.")
        case "findMoments":
            handoff.momentsQuery = action.query
            handoff.navigate(.moments)
            phases[action.id] = .done("Opened Search & Moments with this search.")
        case "exportFiles":
            guard let store = handoff.store, let session = handoff.toolsSession, let ids = action.fileIDs else { return }
            let rows = await Task.detached(priority: .userInitiated) { store.files(forFileIDs: ids) }.value
            session.hits = rows.map { CatalogHit(fileID: $0.id, path: $0.pathText, kind: $0.kind, text: "") }
            session.selection = Set(rows.map(\.id))
            handoff.navigate(.tools)
            phases[action.id] = .done("Opened File Tools with \(rows.count.formatted()) files chosen. Pick a format there.")
        case "changeSetting":
            guard let setting = action.setting, let value = action.value else { return }
            let previous = Self.setting(setting)
            Self.setSetting(setting, value)
            phases[action.id] = .done("Turned \(value ? "on" : "off").")
            offerUndo(action, label: "Undo") { Self.setSetting(setting, previous); return "Changed back." }
        case "suggestFolderLayout":
            guard let path = action.path else { return }
            let signal = engine.restructurePlanSignal
            guard engine.planRestructure(libraryRoot: path) else { phases[action.id] = .failed("FileID is still getting ready. Try again in a moment."); return }
            handoff.navigate(.restructure)
            phases[action.id] = .running("Planning a tidier layout")
            await watch(action, started: { true }, finished: { engine.restructurePlanSignal != signal }) { [weak self] in
                guard let summary = self?.planSummary(action, engine: engine) else { return "Nothing needs to move." }
                return "Layout ready: \(summary.count.formatted()) files would move. Nothing has moved yet; review it in Organize Folders or ask Chat to apply it."
            }
        case "applyFolderLayout":
            guard let summary = planSummary(action, engine: engine) else {
                phases[action.id] = .failed("There's no suggested layout for this folder yet. Ask Chat to organize it first.")
                return
            }
            let signal = engine.restructureApplyResultSignal
            guard engine.applyRestructure(libraryRoot: summary.root, moves: summary.moves, planID: summary.planID) else {
                phases[action.id] = .failed("FileID is still getting ready. Try again in a moment."); return
            }
            phases[action.id] = .running("Moving files")
            await watch(action, started: { true }, finished: { engine.restructureApplyResultSignal != signal }) {
                guard let result = engine.restructureApplyResult else { return "Done." }
                if result.cancelled { return "Stopped after moving \(result.applied.formatted()) files." }
                return "Moved \(result.applied.formatted()) files\(result.failed > 0 ? " · \(result.failed.formatted()) couldn't move" : "")."
            }
            if engine.canUndoRestructure {
                offerUndo(action, label: "Undo moves") {
                    let signal = engine.restructureApplyResultSignal
                    guard engine.undoRestructure(libraryRoot: summary.root) else { return "Undo didn't start. Try Organize Folders." }
                    while engine.restructureApplyResultSignal == signal { try? await Task.sleep(for: .milliseconds(300)) }
                    return "Moved \(engine.restructureApplyResult?.applied.formatted() ?? "the") files back."
                }
            }
        case "undoLastMove":
            guard let path = action.path, engine.canUndoRestructure else {
                phases[action.id] = .failed("There's no applied layout to undo."); return
            }
            let signal = engine.restructureApplyResultSignal
            guard engine.undoRestructure(libraryRoot: path) else { phases[action.id] = .failed("FileID is still getting ready. Try again in a moment."); return }
            phases[action.id] = .running("Moving files back")
            await watch(action, started: { true }, finished: { engine.restructureApplyResultSignal != signal }) {
                "Moved \(engine.restructureApplyResult?.applied.formatted() ?? "the") files back."
            }
        case "moveDuplicatesToTrash":
            await trashDuplicates(action)
        case "renameToSuggested":
            guard let store = handoff.store, let renames = action.renames else { return }
            phases[action.id] = .running("Renaming \(renames.count.formatted()) files")
            let wanted = Dictionary(renames.map { ($0.fileID, $0.newName) }, uniquingKeysWith: { first, _ in first })
            let result = await Task.detached(priority: .userInitiated) { () -> ReadStore.BulkRenameResult in
                // Only files whose suggestion is still the one the card showed.
                let rows = store.files(forFileIDs: Array(wanted.keys)).filter { row in
                    guard let proposed = row.vlmProposedName?.trimmingCharacters(in: .whitespacesAndNewlines), let name = wanted[row.id] else { return false }
                    let ext = row.url.pathExtension
                    return (ext.isEmpty ? proposed : "\(proposed).\(ext)") == name
                }
                let result = store.applyProposedNamesBulk(rows)
                BulkRenameSheet.saveLastBatch(result.renamed)
                return result
            }.value
            store.notifyChanged()
            let skipped = renames.count - result.renamed.count - result.failed
            phases[action.id] = .done("Renamed \(result.renamed.count.formatted()) files" + (result.failed > 0 ? " · \(result.failed.formatted()) failed" : "") + (skipped > 0 ? " · \(skipped.formatted()) skipped because their suggestion changed" : "") + ".")
            if !result.renamed.isEmpty {
                let batch = result.renamed
                offerUndo(action, label: "Undo renames") {
                    let undone = await Task.detached(priority: .userInitiated) { store.undoRenames(batch) }.value
                    if undone.failed == 0 && undone.skipped == 0 { BulkRenameSheet.clearLastBatch() }
                    store.notifyChanged()
                    return "Put back \(undone.undone.formatted()) names."
                }
            }
        case "namePerson":
            guard let store = handoff.store, let id = action.personID, let name = action.name else { return }
            guard let before = store.persons(includeUnknown: true).first(where: { $0.id == id }) else { phases[action.id] = .failed("That person is no longer in People."); return }
            let parts = name.split(separator: " ").map(String.init)
            store.updatePerson(id: id, title: nil, firstName: parts.first, middleName: nil,
                               lastName: parts.count > 1 ? parts.dropFirst().joined(separator: " ") : nil, suffix: nil, isUnknown: false)
            phases[action.id] = .done("Named this person \(name).")
            offerUndo(action, label: "Undo") {
                store.updatePerson(id: id, title: before.title, firstName: before.firstName, middleName: before.middleName,
                                   lastName: before.lastName, suffix: before.suffix, isUnknown: before.isUnknown)
                return "Name put back."
            }
        case "mergePeople":
            guard let store = handoff.store, let target = action.personID, let sources = action.personIDs else { return }
            phases[action.id] = .running("Merging")
            let moved = await Task.detached(priority: .userInitiated) { store.mergePersons(target: target, sources: sources) }.value
            phases[action.id] = moved == nil ? .failed("The merge didn't go through. Nothing changed.") : .done("Merged · \((moved ?? 0).formatted()) faces moved.")
        default:
            phases[action.id] = .failed("This action isn't available.")
        }
    }

    private func trashDuplicates(_ action: ChatAction) async {
        guard let store = ChatHandoff.shared.store, let victims = action.fileIDs, let keepers = action.keeperFileIDs, victims.count == keepers.count else { return }
        phases[action.id] = .running("Checking copies")
        let rows = await Task.detached(priority: .userInitiated) { store.files(forFileIDs: Array(Set(victims + keepers))) }.value
        let byID = Dictionary(rows.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        var work: [DuplicateTrashItem] = []
        var missing = 0
        for (victim, keeper) in zip(victims, keepers) {
            guard let file = byID[victim], let kept = byID[keeper] else { missing += 1; continue }
            work.append(DuplicateTrashItem(id: file.id, url: file.url, size: file.sizeBytes, exactKeeper: kept.url))
        }
        let total = work.count
        let id = action.id
        let results = await ExactDuplicateVerifier.trash(work) { [weak self] finished in
            self?.phases[id] = .running("Moving to Trash · \(finished.formatted()) of \(total.formatted())")
        }
        let trashed = results.filter(\.success)
        let freed = ByteCountFormatter.string(fromByteCount: trashed.reduce(0) { $0 + $1.size }, countStyle: .file)
        let changed = results.filter(\.verificationFailed).count + missing
        let failed = results.filter { !$0.success && !$0.verificationFailed }.count
        let ids = trashed.map(\.id)
        _ = await Task.detached(priority: .userInitiated) { store.deleteFiles(ids: ids) }.value
        var summary = "Moved \(trashed.count.formatted()) \(trashed.count == 1 ? "copy" : "copies") to Trash · freed \(freed)"
        if changed > 0 { summary += " · \(changed.formatted()) skipped because the file changed since the scan" }
        if failed > 0 { summary += " · \(failed.formatted()) couldn't be moved" }
        phases[action.id] = .done(summary + ". To undo, open Trash and choose Put Back.")
    }

    private func offerUndo(_ action: ChatAction, label: String, _ undo: @escaping () async -> String) {
        undos[action.id] = undo
        undoLabels[action.id] = label
    }

    /// Updates progress until `finished`, then records the result. Gives a
    /// job up to 5 s to report that it started.
    private func watch(_ action: ChatAction, started: @escaping () -> Bool, finished: @escaping () -> Bool, result: @escaping () -> String) async {
        let deadline = Date().addingTimeInterval(5)
        while !started(), Date() < deadline { try? await Task.sleep(for: .milliseconds(200)) }
        while !finished() { try? await Task.sleep(for: .milliseconds(300)) }
        if case .running = phase(action) { phases[action.id] = .done(result()) }
    }

    // MARK: - Settings Chat may change

    static func setting(_ key: String) -> Bool {
        let defaults = UserDefaults.standard
        switch key {
        case "cleanupAutoTag": return defaults.object(forKey: AppSettings.cleanupAutoTagKey) == nil ? AppSettings.cleanupAutoTagDefault : defaults.bool(forKey: AppSettings.cleanupAutoTagKey)
        case "backgroundMotion": return (defaults.string(forKey: "lavaMode") ?? LavaMode.full.rawValue) != LavaMode.off.rawValue
        default: return defaults.object(forKey: "chatUseModel") == nil ? true : defaults.bool(forKey: "chatUseModel")
        }
    }

    static func setSetting(_ key: String, _ value: Bool) {
        let defaults = UserDefaults.standard
        switch key {
        case "cleanupAutoTag": defaults.set(value, forKey: AppSettings.cleanupAutoTagKey)
        case "backgroundMotion": defaults.set(value ? LavaMode.full.rawValue : LavaMode.off.rawValue, forKey: "lavaMode")
        case "chatSummaries": defaults.set(value, forKey: "chatUseModel")
        default: break
        }
    }
}

/// Client-side copy of the engine's action rules, so a reply that skipped
/// confirmation or carries an unknown kind can never run.
enum ChatActions {
    static let confirming: Set<String> = ["moveDuplicatesToTrash", "applyFolderLayout", "undoLastMove", "renameToSuggested", "namePerson", "mergePeople"]

    static func isRunnable(_ action: ChatAction) -> Bool {
        ChatAction.kinds.contains(action.kind) && action.requiresConfirmation == confirming.contains(action.kind)
            && (action.page.map(ChatAction.pages.contains) ?? true) && (action.setting.map(ChatAction.settings.contains) ?? true)
    }

    static func pageTitle(_ page: String) -> String {
        switch page {
        case "people": "People"
        case "cleanup": "Duplicates"
        case "deepAnalyze": "Describe Files"
        case "restructure": "Organize Folders"
        case "settings": "Settings"
        case "search": "Search & Moments"
        case "tools": "File Tools"
        case "chat": "Chat"
        default: "Library"
        }
    }

    static func icon(_ kind: String) -> String {
        switch kind {
        case "scanFolder", "rescan": "folder.badge.plus"
        case "describeFiles": "text.below.photo"
        case "stopJob": "stop.circle"
        case "findDuplicates", "moveDuplicatesToTrash": "trash"
        case "suggestFolderLayout", "applyFolderLayout", "undoLastMove": "rectangle.3.offgrid"
        case "renameToSuggested": "character.cursor.ibeam"
        case "namePerson", "mergePeople": "person.crop.circle"
        case "findMoments": "sparkle.magnifyingglass"
        case "exportFiles": "wand.and.stars"
        case "search": "magnifyingglass"
        case "changeSetting": "gearshape"
        default: "arrow.up.right.square"
        }
    }
}

/// One proposed action in the conversation: what will happen, the primary
/// button, Cancel, live progress and the result with Undo where available.
struct ChatActionCard: View {
    let action: ChatAction
    let engine: EngineClient
    @State private var runner = ChatActionRunner.shared

    private var phase: ChatActionRunner.Phase { runner.phase(action) }
    private var destructive: Bool { action.kind == "moveDuplicatesToTrash" }
    private var accent: Color { destructive ? Theme.danger : action.requiresConfirmation ? Theme.gold : Theme.info }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: ChatActions.icon(action.kind)).font(.system(size: 12, weight: .semibold)).foregroundStyle(accent)
                    .accessibilityHidden(true)
                SectionLabel(action.requiresConfirmation ? "Needs your OK" : "Suggested", color: accent)
                Spacer(minLength: 0)
                if action.source == "model" { MetaText("Suggested by local AI", color: Theme.ai, size: 11.5) }
            }
            Text(cardDetail).font(Theme.Font.body(13)).foregroundStyle(Theme.text1).fixedSize(horizontal: false, vertical: true)
            if action.requiresConfirmation, phase == .proposed { whatHappens }
            status
            buttons
        }
        .padding(12)
        .background(Color.white.opacity(0.035), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(accent.opacity(0.35), lineWidth: 1))
        .animation(Theme.springStandard, value: phase)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(action.requiresConfirmation ? "Needs your OK" : "Suggested action"): \(primaryTitle)")
    }

    private var plan: ChatActionRunner.PlanSummary? {
        action.kind == "applyFolderLayout" ? runner.planSummary(action, engine: engine) : nil
    }

    private var cardDetail: String {
        if action.kind == "applyFolderLayout" {
            guard let plan else { return "There's no suggested layout for this folder yet. Ask Chat to organize it first; nothing moves until you apply it." }
            return "Moves \(plan.count.formatted()) files in \(URL(fileURLWithPath: plan.root).lastPathComponent) into the suggested folders. Files the plan marked for review stay where they are."
        }
        if action.kind == "undoLastMove", !engine.canUndoRestructure { return "There's no applied layout to undo." }
        return action.detail
    }

    private var primaryTitle: String {
        if action.kind == "applyFolderLayout", let plan { return "Move \(plan.count.formatted()) files" }
        return action.title
    }

    private var whatHappens: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 14) {
                if let count = plan?.count ?? action.fileCount { fact("Files", count.formatted()) }
                if let bytes = action.byteCount { fact("Size", ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)) }
            }
            let examples = plan?.examples ?? action.examples ?? []
            if !examples.isEmpty {
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(Array(examples.prefix(3).enumerated()), id: \.offset) { _, example in
                        Text(example.hasPrefix("/") ? URL(fileURLWithPath: example).lastPathComponent : example)
                            .font(Theme.Font.mono(11.5)).foregroundStyle(Theme.text2).lineLimit(1).truncationMode(.middle)
                            .help(example)
                    }
                    if (plan?.count ?? action.fileCount ?? 0) > examples.prefix(3).count {
                        MetaText("and \(((plan?.count ?? action.fileCount ?? 0) - examples.prefix(3).count).formatted()) more", size: 11.5)
                    }
                }
            }
            if let undo = action.undo { MetaText("Undo: \(undo)", color: Theme.text2, size: 12) }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.well, in: RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous))
    }

    private func fact(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            SectionLabel(label)
            Text(value).font(Theme.Font.mono(12.5, .semibold)).foregroundStyle(Theme.text1)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var status: some View {
        switch phase {
        case .running:
            HStack(spacing: 10) {
                IndeterminateBar().frame(width: 60)
                MetaText(runner.progress(action, engine: engine) ?? "Working…", color: Theme.ai)
            }
            .accessibilityElement(children: .combine)
        case .done(let text):
            Label { Text(text).font(Theme.Font.body(12.5)).foregroundStyle(Theme.text2).fixedSize(horizontal: false, vertical: true) }
                icon: { Image(systemName: "checkmark.circle.fill").foregroundStyle(Theme.delight) }
        case .failed(let text):
            Label { Text(text).font(Theme.Font.body(12.5)).foregroundStyle(Theme.danger).fixedSize(horizontal: false, vertical: true) }
                icon: { Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.danger) }
        case .cancelled:
            MetaText("Cancelled. Nothing changed.")
        case .proposed:
            EmptyView()
        }
    }

    @ViewBuilder private var buttons: some View {
        HStack(spacing: 8) {
            switch phase {
            case .proposed:
                let unavailable = (action.kind == "applyFolderLayout" && plan == nil) || (action.kind == "undoLastMove" && !engine.canUndoRestructure)
                if destructive {
                    Button(primaryTitle) { start() }.buttonStyle(.dsDanger).disabled(unavailable)
                } else {
                    Button(primaryTitle) { start() }.buttonStyle(DSPrimaryButtonStyle()).disabled(unavailable)
                }
                if action.requiresConfirmation {
                    Button("Cancel") { runner.cancel(action) }.buttonStyle(DSSecondaryButtonStyle())
                }
            case .running:
                if runner.canStop(action) {
                    Button("Stop") { runner.stop(action, engine: engine) }.buttonStyle(DSSecondaryButtonStyle())
                }
            case .done:
                if runner.hasUndo(action) {
                    Button(runner.undoLabels[action.id] ?? "Undo") { Task { await runner.undo(action) } }.buttonStyle(DSSecondaryButtonStyle())
                }
                if action.kind == "moveDuplicatesToTrash" {
                    Button("Open Trash") {
                        NSWorkspace.shared.open(FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".Trash"))
                    }
                    .buttonStyle(DSSecondaryButtonStyle())
                }
            case .failed, .cancelled:
                EmptyView()
            }
            Spacer(minLength: 0)
        }
    }

    private func start() {
        Task { await runner.run(action, engine: engine) }
    }
}
