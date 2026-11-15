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
        #expect(Sidebar.errorTitle("never_heard_of_it") == "Something went wrong")
    }
}
