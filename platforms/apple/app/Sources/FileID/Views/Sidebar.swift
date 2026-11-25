// Sidebar: every page in one list (grouped Browse / Analyze / Act), then a compact scan card.
import SwiftUI
import FileIDShared

struct Sidebar: View {
    let engine: EngineClient
    let store: ReadStore
    @Binding var activeTab: MainWindow.Tab
    @Binding var pickedURL: URL?
    var pickFolder: () -> Void

    @State private var shownArea: MainWindow.Area = .browse
    @Namespace private var areaPill

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            areaSwitcher
                .padding(.horizontal, 10).padding(.top, 12).padding(.bottom, 10)
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(shownArea.tabs) { tab in navRow(tab) }
                }
                .padding(.bottom, 10)
                .id(shownArea)
                .transition(.opacity)
            }
            .frame(maxHeight: .infinity, alignment: .top)

            if shouldShowEngineSection {
                VStack(alignment: .leading, spacing: 8) {
                    if engine.startFailure == nil { EngineStatusRow(state: engine.state, onRestart: { engine.restart() }) }
                    if let err = engine.lastError, Self.showsErrorCard(err.kind) { engineErrorRow(err) }
                }
                .padding(.horizontal, 12).padding(.bottom, 10)
            }

            if !engine.queueState.pending.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    SectionLabel("Queue")
                    QueueListView(state: engine.queueState)
                }
                .padding(.horizontal, 14).padding(.bottom, 10)
            }

            if let url = pickedURL {
                folderCard { PipelineBlock(engine: engine, store: store, pickedURL: url, openPage: { activeTab = $0 }) }
            } else if store.totalFiles > 0 || activeTab == .settings {
                folderCard {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Start with a folder").font(Theme.ChromeFont.body(13, .semibold)).foregroundStyle(Theme.text1)
                        Button { pickFolder() } label: { Label("Choose folder…", systemImage: "folder.badge.plus") }
                            .buttonStyle(DSPrimaryButtonStyle(height: 30))
                        MetaText("Or drag a folder onto this window.")
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear { shownArea = Self.areaShown(activeTab: activeTab, lastUsed: .lastUsed()) }
        .onChange(of: activeTab) { _, tab in
            let area = Self.areaShown(activeTab: tab, lastUsed: shownArea)
            if area != shownArea { withAnimation(Theme.quick) { shownArea = area } }
        }
        .background(Theme.bg1.opacity(0.55), ignoresSafeAreaEdges: [])
        .background(.ultraThinMaterial, ignoresSafeAreaEdges: [])
    }

    private func folderCard<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        content()
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.white.opacity(0.035), in: RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
            .padding(10)
    }

    /// Browse / Analyze / Act: picks which pages the list shows and opens the
    /// page last used in that area.
    private var areaSwitcher: some View {
        HStack(spacing: 2) {
            ForEach(MainWindow.Area.allCases) { area in
                let on = shownArea == area
                Button {
                    withAnimation(Theme.spring) { shownArea = area }
                    activeTab = area.remembered(current: activeTab)
                } label: {
                    Text(area.rawValue)
                        .font(Theme.ChromeFont.body(12.5, on ? .semibold : .medium))
                        .foregroundStyle(on ? Theme.onGold : Theme.text2)
                        .frame(maxWidth: .infinity)
                        .frame(height: 26)
                        .background {
                            if on {
                                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                                    .fill(Theme.gold)
                                    .matchedGeometryEffect(id: "pill", in: areaPill)
                            }
                        }
                        .contentShape(Rectangle())
                }
                .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.s))
                .help("\(area.rawValue): \(area.tabs.map(\.title).joined(separator: ", ")) (\(area.shortcutHint))")
                .accessibilityLabel("\(area.rawValue) pages")
                .accessibilityAddTraits(on ? [.isSelected, .isButton] : [.isButton])
            }
        }
        .padding(3)
        .background(Color.black.opacity(0.28), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
    }

    /// The area whose pages the list shows and whose pill is lit: the one holding the open page; on Settings, which no
    /// area holds, the one the person was last in.
    nonisolated static func areaShown(activeTab: MainWindow.Tab, lastUsed: MainWindow.Area) -> MainWindow.Area { activeTab.area ?? lastUsed }

    /// A stopped folder plan explains itself on Organize Folders; the sidebar
    /// keeps its card for errors nothing else explains.
    nonisolated static func showsErrorCard(_ kind: String) -> Bool {
        kind != "plan_restructure_cancelled"
    }

    private var shouldShowEngineSection: Bool {
        if let err = engine.lastError, Self.showsErrorCard(err.kind) { return true }
        switch engine.state {
        case .starting, .crashed: return true
        case .ready:              return false
        }
    }

    /// Short trailing badge; nil when there is nothing worth saying.
    private func badge(for tab: MainWindow.Tab) -> (text: String, color: Color)? {
        switch tab {
        case .library:
            if let p = engine.lastProgress, p.phase == .discovering || p.phase == .tagging {
                let total = max(p.total, p.discovered)
                return (total > 0 ? "\(Int(Double(p.processed) / Double(total) * 100))%" : "…", Theme.gold)
            }
            return store.scopedTotalFiles > 0 ? (compact(store.scopedTotalFiles), Theme.text3) : nil
        case .people:
            if engine.faceClusteringInFlight { return ("…", Theme.ai) }
            if let r = engine.lastFaceClustering, r.personCount > 0 { return (compact(r.personCount), Theme.text3) }
            if store.totalFiles > 0, !isFaceModelInstalled { return ("Setup", Theme.warning) }
            return nil
        case .deep:
            if engine.deepAnalyzeInFlight, let d = engine.deepAnalyzeProgress, d.total > 0 {
                return ("\(Int(Double(d.processed) / Double(d.total) * 100))%", Theme.ai)
            }
            return nil
        case .moments:
            let running = engine.catalogJobs.filter { $0.state == "running" }.count
            return running > 0 ? ("\(running)", Theme.ai) : nil
        case .restructure:
            // A count, like the other badges, and nothing when the plan moves no files.
            guard let plan = engine.restructurePlan else { return nil }
            let moves = plan.totalMoves ?? plan.moves.count
            return moves > 0 ? (compact(moves), Theme.text3) : nil
        default:
            return nil
        }
    }

    private var isFaceModelInstalled: Bool {
        ModelInstaller.face.status.isInstalled
    }

    private func compact(_ n: Int) -> String {
        n >= 10_000 ? String(format: "%.0fk", Double(n) / 1000) : n.formatted()
    }

    @ViewBuilder
    private func navRow(_ tab: MainWindow.Tab) -> some View {
        let on = activeTab == tab
        Button { activeTab = tab } label: {
            HStack(spacing: 10) {
                Image(systemName: tab.systemImage)
                    .font(.system(size: 13, weight: on ? .semibold : .regular))
                    .foregroundStyle(on ? Theme.gold : Theme.text2)
                    .frame(width: 18)
                Text(tab.title)
                    .font(Theme.ChromeFont.body(13.5, on ? .semibold : .regular))
                    .foregroundStyle(on ? Theme.gold : Theme.text1)
                    .lineLimit(1)
                Spacer(minLength: 4)
                if let b = badge(for: tab) {
                    Text(b.text).font(Theme.ChromeFont.body(11.5, .medium)).foregroundStyle(b.color).monospacedDigit()
                }
            }
            .padding(.horizontal, 10)
            .frame(height: 32)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(on ? Theme.gold.opacity(0.12) : Color.clear, in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        }
        .buttonStyle(.dsRow)
        .padding(.horizontal, 8)
        .help(tab.shortcutIndex.map { "\(tab.summary) (⌘\($0))" } ?? tab.summary)
        .accessibilityLabel("\(tab.title) page")
        .accessibilityHint(tab.summary)
        .accessibilityAddTraits(on ? [.isSelected, .isButton] : [.isButton])
        .animation(Theme.quick, value: on)
    }

    /// Readable titles for engine notices; the code stays in the log line, not on screen.
    nonisolated static func errorTitle(_ kind: String, message: String = "") -> String {
        if isMemoryShortage(kind: kind, message: message) { return "This Mac is short on memory" }
        switch kind {
        case "discovery_partial": return "Some files were left out"
        case "empty_folder": return "No files to scan"
        case "scan_failed": return "The scan stopped"
        case "db_unavailable", "db_failed": return "The library couldn’t be opened"
        case "db_newer_than_engine": return "This library needs a newer FileID"
        case "folder_access_denied": return "FileID can’t reach that folder"
        case "command_decode_failed": return "FileID couldn’t do that"
        case "plan_restructure_failed": return "The folder plan didn’t finish"
        case "orphan_sweep_failed", "purge_excluded_failed": return "Cleaning up the library didn’t finish"
        case "model_load_failed", "deep_load_failed": return "An AI model didn’t load"
        case "deep_model_too_big": return "This AI model is too big for this Mac"
        case "deep_source_changed": return "A file changed while it was described"
        case "deep_targets_failed": return "Describing files stopped"
        case "deep_analyze_selection_too_large", "command_limit_exceeded": return "Too many files at once"
        case "model_integrity_failed": return "An AI model download was damaged"
        case "sentinel_attestation_failed", "sentinel_dir_create_failed", "sentinel_rename_failed", "sentinel_write_failed",
             "zip_extract_failed": return "The AI model couldn’t be installed"
        case "plan_restructure_db", "plan_restructure_store", "plan_restructure_invalid": return "The folder plan didn’t finish"
        case "restructure_undo_journal": return "The folder move stopped"
        case "library_busy": return "The scan is paused"
        case "db_write_failed", "face_cluster_persist_failed", "face_print_persist_failed": return "The library couldn’t be saved"
        case "command_handler_panic", "oversized_ipc_frame", "invalid_face_review_request", "invalid_health_request",
             "invalid_list_models_request", "invalid_list_roots_request": return "FileID couldn’t do that"
        case "not_applicable_on_platform": return "Not needed on a Mac"
        case "restructure_busy", "restructure_already_running": return "A folder move is already running"
        case "engine_exited": return "FileID’s helper stopped"
        case "bookmark_unavailable": return "FileID can’t reach that folder"
        case "ipc_frame_too_large": return "Too much to show at once"
        case "vlm_model_missing": return "This AI model isn’t installed"
        case "unknown_model", "prewarm_failed", "models_not_installed", "models_dir_unavailable",
             "llama_cpp_missing": return "The AI model couldn’t be used"
        case "db_open_failed", "db_integrity_check_failed", "plan_restructure_db_unavailable": return "The library couldn’t be opened"
        case "list_roots_failed": return "FileID can’t reach that folder"
        case "plan_restructure_root": return "This folder isn’t available"
        case "plan_restructure_library_busy": return "A folder move is already running"
        case "location_read_only": return "This drive is read-only"
        case "apply_restructure": return "The folder move didn’t finish"
        case "apply_restructure_skipped": return "Some files weren’t moved"
        case "undo_restructure": return "Putting folders back didn’t finish"
        case "scan_already_running": return "A scan is already running"
        case "model_download_disk_full": return "Not enough disk space"
        case "db_disk_full", "scan_disk_full": return "Your disk is full"
        case "deep_analyze_failed": return "Describing files stopped"
        case "face_clustering_failed", "face_clustering_aborted", "face_clustering_start_failed": return "Grouping faces didn’t finish"
        case "find_merge_suggestions_failed": return "Looking for matching people didn’t finish"
        case "face_cluster_no_model", "face_cluster_embedder_load_failed": return "Faces can’t be grouped yet"
        case "engine_output_error": return "FileID’s helper stopped"
        case "ocr_accurate_disabled": return "Careful text reading paused"
        default: return "Something went wrong"
        }
    }

    /// The sentence under the title. Known kinds get fixed wording; anything
    /// else shows FileID's own message unless it reads like a log line.
    nonisolated static func errorText(kind: String, message: String) -> String {
        if isMemoryShortage(kind: kind, message: message) { return "Close a few apps, then try again. The model is installed." }
        switch kind {
        case "scan_failed": return "Press Scan again. Files already read are skipped."
        case "db_unavailable", "db_failed", "db_open_failed", "db_integrity_check_failed", "plan_restructure_db_unavailable":
            return "Quit and reopen FileID. If it keeps happening, use Start over in the folder menu."
        case "db_newer_than_engine": return "This library was made by a newer FileID. Update FileID."
        case "folder_access_denied", "bookmark_unavailable", "list_roots_failed": return "Choose the folder again so FileID can open it."
        case "command_decode_failed", "command_handler_panic", "oversized_ipc_frame", "invalid_face_review_request",
             "invalid_health_request", "invalid_list_models_request", "invalid_list_roots_request":
            return "Try again. If it keeps happening, quit and reopen FileID."
        case "plan_restructure_failed", "plan_restructure_db", "plan_restructure_store", "plan_restructure_invalid":
            return "Open Organize Folders and try again, or pick a smaller folder."
        case "deep_model_too_big": return "Choose a smaller model in Settings."
        case "deep_source_changed": return "Scan the folder again, then describe it."
        case "deep_targets_failed": return "Try again. If it keeps happening, quit and reopen FileID."
        case "deep_analyze_selection_too_large", "command_limit_exceeded": return "Select fewer files and try again."
        case "model_integrity_failed", "sentinel_attestation_failed", "sentinel_dir_create_failed", "sentinel_rename_failed",
             "sentinel_write_failed", "zip_extract_failed": return "Open Settings and download the model again."
        case "apply_restructure_skipped", "location_read_only": return message
        case "restructure_undo_journal": return "Some files may already have moved. Undo them from Organize Folders, check that there’s free disk space, then try again."
        case "library_busy": return "Resume or cancel the scan, then try again."
        case "db_write_failed", "face_cluster_persist_failed", "face_print_persist_failed":
            return "Check that there’s free disk space, then quit and reopen FileID."
        case "not_applicable_on_platform": return "FileID already picks the fastest hardware by itself."
        case "orphan_sweep_failed", "purge_excluded_failed": return "Nothing was lost. Scan again to finish tidying up."
        case "vlm_model_missing": return "Install it from Describe Files, then try again."
        case "model_load_failed", "deep_load_failed", "prewarm_failed", "unknown_model", "models_not_installed",
             "models_dir_unavailable", "llama_cpp_missing": return "Open Settings and install the model again."
        case "restructure_busy", "restructure_already_running", "plan_restructure_library_busy", "scan_already_running":
            return "Wait for it to finish, then try again."
        case "plan_restructure_root": return "Reconnect it, then try again."
        case "model_download_disk_full": return "Free some space on this computer, then press Retry."
        case "db_disk_full": return "Free some space on this Mac (about 1 GB is plenty), then quit and reopen FileID."
        case "scan_disk_full": return "Free some space on this Mac, then scan again. What FileID already saved is kept."
        case "face_clustering_failed", "face_clustering_aborted", "face_clustering_start_failed":
            return "Open People and press Refresh to try again."
        case "face_cluster_no_model": return "Open Settings and download the face model."
        case "face_cluster_embedder_load_failed": return "Quit and reopen FileID, or download the face model again in Settings."
        case "find_merge_suggestions_failed": return "Open People and try Suggested merges again in a moment."
        case "engine_output_error": return "FileID restarts it by itself. If it stays off, quit and reopen FileID."
        case "ipc_frame_too_large": return "That folder plan is too large to show. Try a smaller folder."
        case "ocr_accurate_disabled":
            return "FileID had trouble reading text in some photos and switched to faster reading for the rest of this scan. Scan again later to read them carefully."
        default: return looksTechnical(message) ? "Try again. If it keeps happening, quit and reopen FileID." : message
        }
    }

    /// A model that is installed but was refused for lack of free memory: reinstalling would not help.
    nonisolated static func isMemoryShortage(kind: String, message: String) -> Bool {
        ["model_load_failed", "deep_load_failed"].contains(kind) && message.lowercased().contains("memory")
    }

    nonisolated static func looksTechnical(_ message: String) -> Bool {
        if message.isEmpty || message.count > 220 { return true }
        let lowered = message.lowercased()
        let markers = ["error domain", "sqlite", "nserror", "errno", "0x", "::", "decodingerror", "codingkeys",
                       "keynotfound", "typemismatch", "fatal", "exception", "stack", "thread", "posix", " ipc",
                       "engine", "database", "catalog", "embedding", "onnx", "mlx", "coreml", "pid ", "/users/",
                       "/volumes/", "(code", "code=", "optional(", "{", "}", "_", "\\"]
        return markers.contains { lowered.contains($0) }
    }

    private func engineErrorRow(_ err: EngineError) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.warning)
            VStack(alignment: .leading, spacing: 2) {
                Text(Self.errorTitle(err.kind, message: err.message)).font(Theme.ChromeFont.body(12, .semibold)).foregroundStyle(Theme.text1)
                Text(Self.errorText(kind: err.kind, message: err.message)).font(Theme.ChromeFont.body(11.5)).foregroundStyle(Theme.text3).lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 4)
            DSIconButton("Dismiss error", systemImage: "xmark", tint: Theme.text3) { engine.clearLastError() }
        }
        .padding(8)
        .background(Theme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.warning.opacity(0.35), lineWidth: 1))
    }
}
