import Foundation
import Testing
@testable import FileID

@Suite("Engine notices and error wording")
struct EngineNoticeKindsTests {
    @Test("notices from either engine raise no error card")
    func quietKinds() {
        for kind in ["rescan_no_changes", "scan_already_running", "scan_already_queued", "prewarm_cancelled",
                     "model_license_not_accepted"] {
            #expect(EngineClient.isQuietNotice(kind), "\(kind)")
        }
        for kind in ["scan_failed", "empty_folder", "db_unavailable", "something_new"] {
            #expect(!EngineClient.isQuietNotice(kind), "\(kind)")
        }
    }

    @Test("kinds the Rust engine sends and a person can meet have their own title and sentence")
    func titles() {
        let kinds = ["db_open_failed", "db_integrity_check_failed", "plan_restructure_db_unavailable", "list_roots_failed",
                     "plan_restructure_root", "plan_restructure_library_busy", "apply_restructure", "undo_restructure",
                     "models_not_installed", "vlm_model_missing", "models_dir_unavailable", "llama_cpp_missing",
                     "model_download_disk_full", "deep_analyze_failed", "face_clustering_failed",
                     "face_clustering_aborted", "face_clustering_start_failed", "find_merge_suggestions_failed",
                     "engine_output_error", "scan_already_running"]
        for kind in kinds {
            #expect(Sidebar.errorTitle(kind) != "Something went wrong", "\(kind)")
            let text = Sidebar.errorText(kind: kind, message: "raw::engine_message {x}")
            #expect(!text.contains("raw::"), "\(kind)")
        }
        #expect(Sidebar.errorTitle("plan_restructure_root") == "This folder isn't available")
        #expect(Sidebar.errorText(kind: "plan_restructure_root", message: "") == "Reconnect it, then try again.")
        #expect(Sidebar.errorTitle("face_cluster_no_model") == "Faces can't be grouped yet")
        #expect(Sidebar.errorTitle("face_cluster_embedder_load_failed") == "Faces can't be grouped yet")
        #expect(Sidebar.errorTitle("never_heard_of_it") == "Something went wrong")
    }

    @Test("a folder move that stopped part way does not say nothing was moved")
    func undoJournalWording() {
        #expect(Sidebar.errorTitle("restructure_undo_journal") == "The folder move stopped")
        #expect(Sidebar.errorText(kind: "restructure_undo_journal", message: "x")
                == "Some files may already have moved. Undo them from Organize Folders, check there is free disk space, then try again.")
    }

    @Test("Describe, model install, plan and save failures from either engine have their own title and sentence")
    func describeAndInstallTitles() {
        let kinds = ["deep_load_failed", "deep_model_too_big", "deep_source_changed", "deep_targets_failed",
                     "deep_analyze_selection_too_large", "command_limit_exceeded", "model_integrity_failed",
                     "sentinel_attestation_failed", "sentinel_dir_create_failed", "sentinel_rename_failed",
                     "sentinel_write_failed", "zip_extract_failed", "plan_restructure_db", "plan_restructure_store",
                     "plan_restructure_invalid", "restructure_undo_journal", "library_busy", "db_write_failed",
                     "face_cluster_persist_failed", "face_print_persist_failed", "command_handler_panic",
                     "oversized_ipc_frame", "invalid_face_review_request", "invalid_health_request",
                     "invalid_list_models_request", "invalid_list_roots_request", "not_applicable_on_platform"]
        for kind in kinds {
            #expect(Sidebar.errorTitle(kind) != "Something went wrong", "\(kind)")
            let text = Sidebar.errorText(kind: kind, message: "Could not load qwen3_vl_2b: mlx::core error {x}")
            #expect(!text.contains("mlx::"), "\(kind)")
        }
        #expect(Sidebar.errorTitle("deep_load_failed") == "An AI model didn't load")
        #expect(Sidebar.errorTitle("vlm_model_missing") == "This AI model isn't installed")
        #expect(Sidebar.errorText(kind: "vlm_model_missing", message: "") == "Install it from Describe Files, then try again.")
    }

    @Test("the Swift engine's switch to faster text reading shows a card with its own words")
    func carefulTextReadingPaused() {
        #expect(!EngineClient.isQuietNotice("ocr_accurate_disabled"))
        #expect(Sidebar.showsErrorCard("ocr_accurate_disabled"))
        #expect(Sidebar.errorTitle("ocr_accurate_disabled") == "Careful text reading paused")
        #expect(Sidebar.errorText(kind: "ocr_accurate_disabled", message: "raw::engine {x}")
                == "FileID had trouble reading text in some photos and switched to faster reading for the rest of this scan. Scan again later to read them carefully.")
    }

    @Test("a stopped folder plan and a fall back explain themselves elsewhere, so the sidebar shows no card")
    func cardlessKinds() {
        #expect(!Sidebar.showsErrorCard("engine_fallback"))
        #expect(!Sidebar.showsErrorCard("plan_restructure_cancelled"))
        #expect(!EngineClient.isQuietNotice("plan_restructure_cancelled"))
        #expect(Sidebar.showsErrorCard("deep_load_failed"))
    }
}
