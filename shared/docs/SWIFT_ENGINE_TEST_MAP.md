# Swift engine tests: what each one pins, and what pins it on the Rust side

Written 2026-11-17 (round 14) for the retirement in `SWIFT_ENGINE_RETIREMENT.md` (its section 5.2 said "no per-test mapping was made"). The only code added with it is the Rust tests listed under "Gaps closed"; no engine behaviour changed.

Categories: **a** pinned by a Rust unit test; **b** pinned by a shared conformance case (`shared/test-corpus/conformance/*.json`); **c** about Swift-only code that disappears with the engine, or about shared code that stays and moves to a Swift test target; **d** gap: true of the product and nothing on the Rust side would catch a regression. `a+b` is both; `a*` is pinned except for one sub-invariant named in the row. A row marked "closed round 14" was a `d` and now has a Rust test.

## Counts (after round 16; Areas C and D after round 17)

| Area | Tests | a | a+b / a* | b | c | d (open) |
|---|---|---|---|---|---|---|
| A data safety, restructure, writer, dispatch | 188 | 129 | 1 | 16 | 40 | 2 |
| B scan, decode, residency, IPC sink, models | 196 | 91 | 4 | 1 | 52 | 48 |
| C faces, takes, search, naming, Describe Files | 292 | 243 | 0 | 0 | 32 | 17 |
| D chat, timeline, video, media tools | 117 | 72 | 13 | 2 | 28 | 2 |
| **Total** | **793** | 535 | 18 | 19 | 152 | 69 |

793 rows against 768 `@Test`/`func test` matches in `platforms/apple/Tests/EngineTests` (118 files) plus the 26 in `FileIDAppTests/EngineQueryRepliesTests`; the difference comes from parameterised tests and one test of `VideoFramePoolTimingTests` that sits in `SpeechPermissionTests.swift`.

## How far to trust the rows

Areas A and D: each Rust pin was found by reading the Rust test or conformance case ("(closest)" marks a partial pin). Areas B and C were matched faster: a trailing `~` or the words "not individually verified" mean the Rust test was matched by name, code path and a skim, and was not proven to fail when the invariant breaks. Read those `a` rows as "probably pinned"; a second pass over them is owed before the Swift tests are deleted. A `d` row may also be wrong in the other direction (one mapping agent listed two gaps, ReassignFace, that the Rust handler tests do pin; they are corrected below). Rows marked "not opened" were not checked at all.

## Gaps closed in round 14, ranked

Every test below passes on the plain build and was shown to fail when its guard was broken locally (the break was reverted, never committed). All sit in files that are not digest-locked. Test names are relative to `platforms/windows/src/engine/src/`.

| # | Gap (what a regression would cost) | Rust test | Breakage that failed it |
|---|---|---|---|
| 1 | A rescan wipes a file's tags, named faces or text when the read timed out or failed (data loss) | `pipeline/dbwriter.rs::rescan_preservation_tests` (3 tests) | tag, face and OCR stage gates forced on |
| 2 | Startup after a killed Trash batch drops the wrong catalog rows, or trusts an entry signed with another key | `commands/trash_log.rs::reconcile_tests::startup_drops_only_rows_of_signed_batches_whose_file_is_gone_and_found_in_the_trash` | signature check removed; "file is gone" check removed |
| 3 | A Trash batch is not journaled for every file, repeats an id, or refuses without a reason | `commands/trash_batch_tests.rs` (2 tests) | id de-duplication removed; journal append removed |
| 4 | An interrupted export's cleanup follows a link and deletes the user's file; an interrupted export stays "running" | `commands/tools.rs::tests::restart_cleanup_never_deletes_what_a_stage_path_links_to` | link check removed; state update removed |
| 5 | A stale plan moves or overwrites the wrong file when a path differs only in case | `pipeline/restructure_case_tests.rs` (default volume and a case-sensitive disk image) | exact-compare-only `paths_equal` failed it on the default volume; see the note below |
| 6 | Organize Folders routes files into a folder outside the library root | `pipeline/restructure_semantic.rs::tests::a_folder_outside_the_library_root_is_never_a_routing_target` | root containment check removed |
| 7 | A library wipe leaves rows behind or leaves foreign keys off | `db/mod.rs::tests::wipe_all_empties_every_data_table_and_leaves_foreign_keys_on` | a table skipped; foreign keys left off |
| 8 | Face auto-merge joins two named people, a person marked Unknown, two people the user said are different, or two people who share a photo (wrongly merged people) | `commands/face_clustering_mac_tests.rs::auto_merge_*` (6 tests, with two positive controls) | verdict, shared-photo and named-pair guards removed (the named pair needed two guards removed) |

Notes: (5) on a case-sensitive volume `paths_equal` is not the only guard (the file identity check also refuses the swap), so a break of `paths_equal` alone does not fail the test there; the test pins the outcome. (8) Unknown people are protected by four independent guards; the test pins the outcome and no single guard was shown to fail it, and removing two together still passed.

## Gaps closed in round 16, ranked

Areas A and D. Every test passes on the plain build and with `apple-adapters`, and failed when its guard was broken locally (the break was reverted, never committed), except where noted. Test names are relative to `platforms/windows/src/engine/src/`.

| # | Gap | Rust test | Breakage that failed it |
|---|---|---|---|
| 1 | Trash moved a file before the batch was journaled | `commands/trash_batch_tests.rs::the_journal_names_the_whole_batch_before_the_first_file_moves` (also makes the journal tests take turns: a trim by rename swallowed a parallel append) | journal append moved after the moves |
| 2 | A restore that got some files back forgot the rest | `commands/trash_batch_tests.rs::a_partial_restore_keeps_the_whole_batch_journaled_for_a_retry` (plain macOS build only) | log truncated after a successful restore |
| 3 | A timed-out file stranded in the skip set; images without a vector skipped forever; an older-model vector kept | `pipeline/dbwriter.rs::rescan_preservation_tests` (3 tests), `scan_session.rs::tests::image_clip_gate_keeps_embeddingless_images_out_of_the_skip_set_only` | failed flag not updated on upsert; clip upsert made DO NOTHING; image gate emptied |
| 4 | Scan cancelled part way left the app waiting | `commands/scan_cancel_tests.rs` | cancel request ignored |
| 5 | With no database a scan left the app scanning; two Organize Folders moves at once | `server_fuzz_tests.rs` (2 tests) | terminal phase removed; busy refusal removed (both hung until the 60 s test limit; the tests now carry a 10 s timeout and were not re-run broken) |
| 6 | Folder homogeneity; nearest-neighbour selection; plan counts | `pipeline/restructure_folder_tests.rs`, `pipeline/restructure_semantic.rs::tests::the_k_nearest_selection_matches_a_full_sort_for_several_k`, `commands/restructure_plan_counts_tests.rs` | threshold 0.9; sort removed; auto counting removed |
| 7 | Pragmas | `db/mod.rs::tests::the_writer_and_a_read_connection_run_with_the_documented_pragmas` | synchronous FULL, foreign keys off |
| 8 | Area D: rank fusion, prompt order, summary model kind, chapter abstention, invalid frame times, photo export refusals | `commands/chat.rs::tests` (3), `pipeline/timeline_analysis_tests.rs` (2), `commands/tools.rs::tests::a_photo_export_refuses_video_only_options_and_every_video_choice_it_cannot_honour` | each guard removed |

Still open in A: `vanishedFileGetsNoFailedRow` (a finding: test written and ignored, see its row) and `plannersRespectCaseOnlyPathIdentity` (the planners are inside the digest-locked `restructure.rs`/`restructure_apply.rs` with no seam reachable from a sibling test). Area D items that stay open: Mac video and media export, reframing and stabilization plans, worker timeout and cancel, EXIF orientation and GPS stripping (these need the FFmpeg runtime, a real JPEG decoder run, or Swift adapter code that stays); chapter export after chapters change and passages marked stale (not attempted this round); whitespace-only chat request, stored recipe, moment-reply cap (presentation or small, not attempted).

## Gaps closed in round 17, ranked

Areas C and D. Every Rust test passes on the plain build and failed when its guard was broken locally (reverted, never committed) except where noted. Swift tests are in `platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift` (code that stays in FileIDAppleKit or FileIDVLMCore; the originals are in EngineTests, which is deleted) and were run with `swift test --filter`. Rust paths are relative to `platforms/windows/src/engine/src/`.

| # | Gap | Test | Breakage that failed it |
|---|---|---|---|
| 1 | Describe Files left the old caption, name and VLM tags under a new model's name when the new output was empty (engine fixed: `persist_vlm_results` now replaces what was asked for, as Swift does; user and scan tags untouched) | `pipeline/deep_analyze.rs::tests` (3) | the old COALESCE update; tag replace skipped for an empty list |
| 2 | Passages, coverage and observations not marked stale when a file changes | `commands/catalog.rs::tests::passages_observations_and_coverage_go_stale_when_the_source_changes` | passage line removed from `catalog_invalidate` |
| 3 | A chapter replaces another file's chapter; bad times or titles stored | `commands/catalog.rs::tests::a_chapter_cannot_replace_another_files_chapter_and_bad_times_are_refused` | owner check, finite check, title trim (each) |
| 4 | Face auto-merge: mixed caches, named bridge, title-only names, search budgets, exact thresholds, changed photo, person limit in `apply()`, unknown window, no-work count, oversized evidence, failed persist | `commands/face_clustering_mac_tests.rs` (12) | each guard named in its row; the bridge needs two guards removed; oversized evidence pins the outcome |
| 5 | Face cache: identity, per-model evidence, hand-edited observation, failure for an edited box, malformed vector | `commands/face_extract_mac_tests.rs` (4) | `person_id` cleared on persist; v22 `user_edited` guard; box match; norm check |
| 6 | Vector index checkpoint after a rolled-back write | `catalog_index_tests.rs::a_rolled_back_vector_write_does_not_advance_the_index_checkpoint` | outcome only (the revision lives in triggers that roll back) |
| 7 | Chapter export after the chapters change; stored recipe; EXIF orientation and location on a photo export | `commands/tools.rs::tests` (3) | marker comparison removed; recipe written as "{}"; `apply_orientation` removed |
| 8 | Speech edge cases, moment reply cap, blank chat request | `speech_passages.rs`, `timeline_analysis_tests.rs`, `chat.rs` | finite check; end clamps; cap moved by one; trim removed |
| 9 | Baseline backfill of existing faces; dense-cluster recall; a reply without TAGS | `db/migrations.rs`, `util/hnsw_index.rs`, `pipeline/deep_analyze_naming.rs` | backfill INSERT removed; ef_search 1; FILENAME dropped from the description end labels |
| 10 | Mac reframing and stabilization plans, video export (audio, timing, frames, padding, enlargement, location), rotation, keyframe time | `AppleKitTests/MediaPlanTests.swift` (14 tests) | gap check in `MediaReframing.plan`; `MediaStabilization.alignment` returning nil; rotation sign; keyframe at 50%. Location stripping is outcome only (AVAssetExportSession dropped it with the filter removed) |

Second pass over 16 Area C `a` rows (wrong person, wrong file, lost data): each named Rust test failed when its guard was removed, and the row says which guard. `FaceClusterPersistenceTests.failedPublicationRollsBackAssignmentsAndAnalysis` was re-pinned: its old pin ran inline SQL only. Three rows the mapping called gaps were already pinned (the 60-character budget, the evidence prompt, an empty result as a failure) and are corrected; the video keyframe row is now `c` (the Swift function stays in FileIDVLMCore and its test moved to AppleKitTests).

Still open in C (17): clustering cancel (2; Rust clustering has no cancel), the persist re-read under the writer lock (no seam between plan and commit), the three auto-merge preflight caps (inline, no seam), the take outcome evidence pipeline (7; no Rust counterpart found), cast hits (2; no Rust feature), moments crowding a Library result and concurrent index refresh, sound-label naming and its deadline (no Rust sound analysis). Still open in D (2): Mac worker timeout and cancel (`self_worker` exists only in the `apple-adapters` build and shares FILEID_TEST_ENGINE_PATH with the Mac media tests; `native_process.rs` is digest-locked).

## Gaps still open, ranked

Data loss or wrong file operations first.

1. `CrashSafetyTests.trashJournalsEachFileBeforeTheNext`: closed in round 16 (a `#[cfg(test)]` hook in `bulk.rs`).
2. `CrashSafetyTests.restoreKeepsTheJournalUntilEachFileIsBack`: closed in round 16 (plain macOS build).
3. `RestructureTests.plannersRespectCaseOnlyPathIdentity`: open. `mixedHomogeneityDominantPerson` and `matchesReferenceForSeveralK`: closed in round 16.
4. `DBWriterClipBackfillTests` (3) and `timeoutFileIsRetriedNotStranded`: closed in round 16. `vanishedFileGetsNoFailedRow`: the Rust writer does not hold it (ignored test, see its row). Open.
5. `cancelMidScanCompletes`, `startScanDbUnavailableEmitsTerminal`, `restructureReservationIsExclusive`, `restructurePlanConfidenceCounts`: closed in round 16 (the last is `a*`).
6. Area B: the discovery skip predicate (an edited file with a backdated modification time keeps stale tags), terminal events under a progress flood, twin reuse rules, the benchmark that must not download. Open.
7. Area C: Describe Files persisting over old tags, vector index checkpoint after a rolled-back write, chapter validation, the 8,000-person cap, mixed face caches, the remaining auto-merge guards (transitive bridge, title-only names, search budget): closed in round 17.
8. Area D: chapter export after chapters change, passages not marked stale, photo EXIF and GPS stripping, reframing and stabilization plans and the Mac video export results: closed in round 17 (the Swift ones moved to AppleKitTests). Open: Mac video export execute and undo once the cross-engine cases lose their Swift peer (conformance conversion, not attempted), Mac worker timeout and cancel.

The full ranked list per area is at the end of each area section below.


## Area A: data safety, restructure, catalog writer, dispatch


Worktree /Users/adamnolle/FileIDDev/Worktrees/test-map. Cat: a = Rust unit test, b = shared conformance case, c = Swift-only (disappears), d = gap. Rust paths are relative to the worktree under `platforms/windows/src/engine/src/`.
Strictness note: a/b were assigned only after reading the Rust test or case body; "(closest)" marks partial pins. Mixed rows list the primary pin first.
Counts: a=106, b=16, c=40, d=26 (188 tests; grep -c '@Test' per suite verified: Bulk 13, BulkCommandID 3, CrashSafety 17, EngineTrash 1, TakeCopyTrash 1, PersonMergeUndo 8, RestructureApplyCancel 1, Feedback 4, RespectPlan 4, Respect 9, Semantic 15, Restructure 36, MigrationParity 4, DatabaseWriterGuard 2, ClipBackfill 7, ModifiedTime 2, RenameHeal 3, StageGate 3, Upsert 3, ScanRootGuard 3, FileSystemChanges 1, LibraryRoots 7, SecurityScoped 3, DispatchHandlers 7, ScanCancellation 5, EngineQueryReplies 26). Not in the list because they do not exist as separate files: DBWriter suites are the five listed; no other Restructure*/DBWriter* files besides IPCSinkRestructurePlanPinTests (not in scope).


### BulkMutationTests (13)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| BulkMutationTests | renameRefusesRowWithoutIdentity | A rename of a catalog row with no file_ref is refused and the file is untouched. | b | conformance files-rename-refuses-row-without-identity (also commands/bulk.rs::held_rename_requires_exact_volume_qualified_identity) |
| BulkMutationTests | personMutationsReportMissingPeople | Renaming or marking-unknown a person that no longer exists reports a per-person failure. | a | platforms/windows/src/engine/src/commands/bulk.rs::removed_person_rename_reports_failure; conformance people-rename-missing-person-fails, people-mark-unknown-missing-person-fails |
| BulkMutationTests | mergeRecordsUserConstraint | An explicit merge records one user-merged edge, drops only the contradicting different-people verdict, and allows faceless people. | a | platforms/windows/src/engine/src/commands/bulk.rs::manual_merge_persists_one_edge_and_removes_only_direct_conflicts, ::manual_merge_of_a_faceless_person_records_no_edge; conformance people-merge-records-user-constraint |
| BulkMutationTests | renameChunk | A bulk rename rewrites path_text, path_hash and extension, and an invalid or missing entry fails without blocking the others. | b | conformance files-rename-updates-path-identity (also platforms/windows/src/engine/src/commands/bulk.rs::rename_batch_persists_each_file_independently) |
| BulkMutationTests | renameAppliesSuggestedName | Renaming to the suggested name clears the suggestion, a case-only rename lands, other renames keep it. | a | platforms/windows/src/engine/src/commands/bulk.rs::rename_allows_case_only_and_clears_the_applied_suggestion |
| BulkMutationTests | suggestedNameMatch | The suggested-name match accepts the name and its numbered bumps only (Rust caps bumps at _99; Swift rule is looser). | a | platforms/windows/src/engine/src/commands/bulk.rs::applied_suggestion_matches_the_name_or_its_bump |
| BulkMutationTests | renameRejectsReplacement | A rename is refused when the file at the path is not the one indexed (file identity mismatch). | a | platforms/windows/src/engine/src/commands/bulk.rs::a_failed_member_rolls_back_its_group (second half: file_ref bumped, "source was replaced" refusal, no file moved) |
| BulkMutationTests | sidecarsFollowRename | Uncatalogued sidecars follow a complete companion group on rename and come back on the inverse rename; hidden files never move. | b | conformance files-rename-sidecars-follow-their-group (also platforms/windows/src/engine/src/commands/bulk.rs::uncatalogued_sidecars_follow_a_complete_group_and_come_back_on_undo) |
| BulkMutationTests | failedMemberRollsBackGroup | If one member of a companion group fails to rename, the whole group is rolled back and reported failed. | b | conformance files-rename-group-rolls-back (also platforms/windows/src/engine/src/commands/bulk.rs::a_failed_member_rolls_back_its_group) |
| BulkMutationTests | trashIDsAreOrderedUnique | Duplicate ids in a trash request are processed once in first-seen order. | a (closed round 14; was d) | platforms/windows/src/engine/src/commands/trash_batch_tests.rs::trash_ids_are_unique_in_first_seen_order_and_refusals_carry_a_reason. Was: none: Rust dedups with a HashSet retain in commands/bulk.rs (trash handler) but no test duplicates an id |
| BulkMutationTests | markPersonsDifferentPersistsStableAnchors | A different-people verdict persists the stable face anchors (face ids, file ids, bboxes). | b | conformance people-mark-different-records-a-verdict-and-refuses-stale-anchors |
| BulkMutationTests | pathPrefetchChunks | Fetching row state for 1,005 ids crosses the SQLite parameter chunk size without omissions. | c | Rust loads rows per id (no SQLite parameter-limit chunking); Swift fetchBulkStates chunking only |
| BulkMutationTests | wipeEmptiesCatalog | Wipe empties every table of a library (catalog, search rows, vector changes), keeps foreign keys enforced and re-seeds vector state. | a (closed round 14; was d) | platforms/windows/src/engine/src/db/mod.rs::tests::wipe_all_empties_every_data_table_and_leaves_foreign_keys_on. Was: platforms/windows/src/engine/src/db/mod.rs::wipe_all_keeps_the_vector_state_the_triggers_need and ::wipe_all_empties_every_fts_kind cover vector state and FTS only; nothing asserts every other table is empty or that foreign_keys stays ON |

### BulkCommandIDTests (3)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| BulkCommandIDTests | rejectionCarriesID | A refusal for an unavailable database carries the command id. | a | platforms/windows/src/engine/src/server.rs::a_refused_bulk_command_echoes_its_id |
| BulkCommandIDTests | successPathCarriesID | A result produced by the database carries the command id. | a | platforms/windows/src/engine/src/server.rs::a_bulk_result_from_a_spawned_task_echoes_its_id; conformance people-merge-result-echoes-the-command-id |
| BulkCommandIDTests | codingRoundTrip | An encoded result carries commandID and a result without it still decodes. | a | platforms/windows/src/engine/src/server.rs::the_sink_keeps_a_result_id_it_already_has_and_leaves_other_events_alone (decode of a result without commandID: ipc generated serde, schema check) |

### CrashSafetyTests (17)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| CrashSafetyTests | trashJournalsEachFileBeforeTheNext | trashFiles journals every file ahead of its move so a kill leaves each file restorable or findable. | a (closed round 14; was d) | platforms/windows/src/engine/src/commands/trash_batch_tests.rs::the_recovery_journal_names_every_file_of_a_batch_even_when_its_move_fails (pins that the journal names every item with its identity even when the move fails; strict write-before-move ordering has no seam and stays open). Was: none: platforms/windows/src/engine/src/commands/bulk.rs appends the batch entry before moves (bulk.rs ~1338) and macOS receipts after (bind_macos_trash_receipts) but no unit test asserts journal-before-move order; shared/scripts/e2e/crash_safety.py is a manual harness, not a gate ; round 16: `commands/trash_batch_tests.rs::the_journal_names_the_whole_batch_before_the_first_file_moves` reads the journal and the catalog from a `#[cfg(test)]` hook (`bulk.rs::BEFORE_TRASH_MOVE`) called just before each file is handed to the Trash backend: the whole batch is journaled and no row is dropped yet. Fails when the append is moved after the moves. |
| CrashSafetyTests | restoresARustBatch | Swift restoreFromTrash restores a batch the Rust engine journaled, with its catalog snapshot. | c | Swift RustTrashLog reader (deleted with the engine); the Rust log itself: platforms/windows/src/engine/src/commands/trash_log.rs::appended_entry_is_findable_by_batch_id |
| CrashSafetyTests | trashDualWritesTheRustJournal | Swift trashFiles dual-writes the signed Rust-format journal. | c | Swift dual-write to the Rust log (RustTrashLog.swift); Rust side is the writer: platforms/windows/src/engine/src/commands/trash_log.rs::entry_serde_round_trip |
| CrashSafetyTests | exactDuplicateTrashIsVerifiedAndUndoable | Cleanup exact duplicates are re-proved byte-identical by the engine before they are trashed (journaled and restorable). | a | platforms/windows/src/engine/src/commands/bulk.rs::exact_trash_evidence_rejects_same_size_in_place_changes_and_restores_claim, ::exact_trash_rejects_individually_valid_but_unequal_proof, ::exact_trash_locks_keeper_and_victim_through_backend_call, ::trashing_one_copy_leaves_the_kept_byte_identical_copy_on_disk |
| CrashSafetyTests | restoreKeepsTheJournalUntilEachFileIsBack | A restore keeps its batch journaled while files are moving back, so a kill never forgets files still in the Trash. | a (closed round 16; was d) | platforms/windows/src/engine/src/commands/trash_batch_tests.rs::a_partial_restore_keeps_the_whole_batch_journaled_for_a_retry (plain macOS build: one file back, one away; the batch stays readable with both items; a retry restores both) |
| CrashSafetyTests | restoreRetryFinishesAFileAlreadyMovedBack | A retried restore finishes a file a killed restore already moved home (re-inserts its rows). | a | platforms/windows/src/engine/src/commands/trash.rs::restored_catalog_reconciliation_is_exact_and_idempotent (already_restored_outcome + reconcile twice) |
| CrashSafetyTests | restoreRetryRefusesAnUnrelatedOccupant | A retried restore never overwrites an unrelated file now at the original path. | a | platforms/windows/src/engine/src/commands/trash.rs::occupied_destination_is_a_conflict_not_success, ::intent_journal_claim_is_restored_without_clobbering (windows/linux cfg) |
| CrashSafetyTests | toolsUndoRemovesAnOutputPublishedBeforeACrash | Undo of an export removes an output that was published just before a kill (receipt pending). | a | platforms/windows/src/engine/src/commands/tools.rs::undo_published_receipts_recover_after_restart_and_cleanup_or_unlink_failure, ::undo_changed_published_recovery_is_not_deleted_or_claimed_complete |
| CrashSafetyTests | vanishedFileGetsNoFailedRow | A file deleted mid-scan after discovery gets no new catalog row. | d (finding, round 16) | none: pipeline/dbwriter.rs has no test for a file that vanishes between discovery and write (Swift audit B2 notes Rust keeps rows as failed=1); no conformance case | ; round 16: `pipeline/dbwriter.rs::rescan_preservation_tests::a_file_deleted_after_discovery_gets_no_failed_row_but_a_broken_one_does` is written and `#[ignore]`d: the Rust writer DOES store a failed row for a file deleted after discovery (the Swift writer skips it). The fix is about ten lines in `dbwriter.rs::flush` (skip a failed record whose path is gone and has no row); it was not applied because the permission check refused the edit.
| CrashSafetyTests | undoRetryAcceptsAFileAlreadyMovedBack | A retried restructure undo accepts a file a killed undo already moved home. | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::undo_retry_treats_an_already_restored_entry_as_idempotent, ::undo_reconciles_a_completed_move_after_db_update_failure_via_reconcile_pass |
| CrashSafetyTests | killRightAfterTheMoveStillRestores | A file trashed just before a kill is restorable by identity. | a | platforms/windows/src/engine/src/shell/macos.rs::tests::folder_trash_uses_finder_names_and_restores_by_inode, platforms/windows/src/engine/src/commands/trash_log.rs::startup_puts_a_stranded_trash_claim_back (restorable by identity; the write-ahead-before-move half is the gap in row 1) |
| CrashSafetyTests | reconcileReadsTheRustLogAfterAKill | Startup drops catalog rows for files a killed Rust Trash batch already moved, reading its log read-only and ignoring an entry signed with another key. | a (closed round 14; was d) | platforms/windows/src/engine/src/commands/trash_log.rs::reconcile_tests::startup_drops_only_rows_of_signed_batches_whose_file_is_gone_and_found_in_the_trash. Was: none: platforms/windows/src/engine/src/commands/trash_log.rs::reconcile_trashed_rows (macOS startup repair, HMAC-signed entries only) has zero tests; unsigned_claim_record_is_ignored covers claims, not row reconciliation |
| CrashSafetyTests | killBeforeTheMoveLeavesTheFileInPlace | A write-ahead entry whose file never moved is completed as already home. | c | Swift Trash journal replay (TrashJournal.swift); Rust uses claim files, nothing is journaled-but-unmoved to complete |
| CrashSafetyTests | renameKilledBeforeTheCatalogUpdateIsRepaired | A rename killed between the move and the catalog update is repaired at startup. | a | platforms/windows/src/engine/src/commands/path_moves.rs::rust_intents_round_trip_through_repair, ::pending_swift_rename_is_repaired_and_the_journal_cleared |
| CrashSafetyTests | renameIntentWithoutTheMoveIsIgnored | A rename intent whose file never moved, or moved somewhere else, changes nothing. | a | platforms/windows/src/engine/src/commands/path_moves.rs::rename_to_a_different_file_is_not_repaired, ::rust_intents_round_trip_through_repair (never-moved intent) |
| CrashSafetyTests | finishedRenameClearsItsIntent | A finished rename leaves nothing pending. | a | platforms/windows/src/engine/src/commands/path_moves.rs::rust_intents_round_trip_through_repair (settled rename, journal removed) |
| CrashSafetyTests | testStateIsIsolated | Tests never write engine state into the real Application Support folder. | c | Swift test-hygiene guard on its own state folder |

### EngineTrashTests (1)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| EngineTrashTests | ordinaryBuildsUseTheUsersTrash | Ordinary builds use the user's Trash, never a test Trash root. | c | Swift EngineTrash test root; Rust equivalent is the test-trash-root cargo feature (shell/macos.rs::tests::unit_tests_never_reach_the_real_trash) |

### TakeCopyTrashTests (1)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| TakeCopyTrashTests | keptCopySurvives | Trashing one of two byte-identical copies leaves the kept copy and its row intact. | a | platforms/windows/src/engine/src/commands/bulk.rs::trashing_one_copy_leaves_the_kept_byte_identical_copy_on_disk |

### PersonMergeUndoTests (8)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| PersonMergeUndoTests | undoRestoresNamesFacesCountsAndVerdicts | Undo restores names faces counts and verdicts | a | platforms/windows/src/engine/src/commands/person_merge_tests.rs::undo_restores_names_faces_counts_and_verdicts; conformance people-merge-undo-restores-the-merged-person |
| PersonMergeUndoTests | namedTargetKeepsItsNameAndSourceNameReturns | Named target keeps its name and source name returns | a | platforms/windows/src/engine/src/commands/person_merge_tests.rs::named_target_keeps_its_name_and_the_source_name_returns |
| PersonMergeUndoTests | undoAfterAFaceWasDeletedMovesTheRestBack | Undo after a face was deleted moves the rest back | a | platforms/windows/src/engine/src/commands/person_merge_tests.rs::undo_after_a_face_was_deleted_moves_the_rest_back |
| PersonMergeUndoTests | facesMovedElsewhereStayAndAnEmptiedMergeIsRefused | Faces moved elsewhere stay and an emptied merge is refused | a | platforms/windows/src/engine/src/commands/person_merge_tests.rs::faces_moved_elsewhere_stay_and_an_emptied_merge_is_refused |
| PersonMergeUndoTests | undoTwiceChangesNothing | Undo twice changes nothing | a | platforms/windows/src/engine/src/commands/person_merge_tests.rs::undo_twice_changes_nothing |
| PersonMergeUndoTests | journalSurvivesReopenAndLastTokenUndoesNewestFirst | Journal survives reopen and last token undoes newest first | a | platforms/windows/src/engine/src/commands/person_merge_tests.rs::journal_survives_reopen_and_the_last_token_undoes_newest_first |
| PersonMergeUndoTests | mergesOlderThanADayAreNotOfferedButStillUndoById | Merges older than a day are not offered but still undo by id | a | platforms/windows/src/engine/src/commands/person_merge_tests.rs::merges_older_than_a_day_are_not_offered_but_still_undo_by_id |
| PersonMergeUndoTests | trashRefusalsCarryAReason | A trash of an unknown file id fails with a reason ("missing") the app can show. | a (closed round 14; was d) | platforms/windows/src/engine/src/commands/trash_batch_tests.rs::trash_ids_are_unique_in_first_seen_order_and_refusals_carry_a_reason. Was: none: reason "missing" is set at commands/bulk.rs:1307 but no test or conformance case asserts a trash refusal carries a reason |

### RestructureTests (36)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| RestructureTests | monthNameFull | Month names are full English names (June, January, December). | b | conformance restructure-apply-and-undo-round-trip and restructure-semantic-passes (Photos/2026/June etc.); platforms/windows/src/engine/src/pipeline/restructure.rs::month_name used by images_routed_to_photos_year_month |
| RestructureTests | categoryVocabulary | Swift wire category strings match the Windows lowercase vocabulary. | c | Swift ruleClassify bucket strings ("photo","misc"...); Rust ProposedMove.category is the source of that vocabulary, nothing to compare to |
| RestructureTests | videoAudioBuckets | A dated video routes to Videos/<Year>, dated audio to Audio/<Year> | a | platforms/windows/src/engine/src/pipeline/restructure.rs::video_year_only, ::audio_year_subfolder, ::dated_audio_gets_its_year |
| RestructureTests | undatedPeopleAndBadCoordinates | A named person or place with no reliable date gets no 1970 folder; bad coordinates fall through | a | platforms/windows/src/engine/src/pipeline/restructure.rs::named_person_without_valid_date_never_creates_a_1970_folder, ::invalid_coordinates_fall_through_instead_of_creating_bogus_places |
| RestructureTests | imageYearMonth | A dated image routes to Photos/<Year>/<MonthName> | a | platforms/windows/src/engine/src/pipeline/restructure.rs::images_routed_to_photos_year_month |
| RestructureTests | missingTimestampYear | A file with no timestamp gets flat folder + Ask confidence | a | platforms/windows/src/engine/src/pipeline/restructure.rs::zero_timestamp_gets_ask_confidence, ::dateless_file_is_ask_and_flat |
| RestructureTests | anchorFolderStrip | Anchor-folder files emit no move proposals (classify + strip) | a | platforms/windows/src/engine/src/pipeline/restructure.rs::anchor_folder_moves_are_stripped_from_plan, ::non_anchor_folder_moves_survive_the_strip; conformance restructure-anchor-strip-exempts-only-claimed-files |
| RestructureTests | mixedHomogeneityDominantPerson | Folder homogeneity is measured against the dominant person, so a folder with one outlier still classifies as Anchor. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/restructure_folder_tests.rs::a_folder_with_one_outlier_is_still_an_anchor_and_an_even_split_is_mixed (Rust measures the dominant destination category; 5/6 is Anchor, 3/6 and 4/6 are Mixed) |
| RestructureTests | personOwnershipNeedsOneNamedPerson | A file belongs to a person only when every face is named and all are one person | a | platforms/windows/src/engine/src/commands/restructure.rs::person_routing_requires_every_face_to_share_one_named_identity, ::plan_query_rejects_named_unknown_and_same_name_identity_ambiguity; conformance restructure-photo-of-several-people-has-no-single-owner |
| RestructureTests | digitFoldersAreJunkPrototypesExceptYears | All-digit export folders are never learn-your-style targets; years are | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::all_digit_folders_are_junk_prototypes_but_years_are_not |
| RestructureTests | applyRefusesUnverifiableRow | A row with neither file_ref nor content hash is refused, not moved | b | conformance restructure-apply-refuses-rows-without-identity |
| RestructureTests | applyUniquifyAndPathHash | Two proposals to one dest both apply (uniquified); path_hash refreshed | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::apply_two_same_basename_sources_keeps_both, ::unique_destination_disambiguates_collisions; path_hash/path_search refresh in update_path_in_db (restructure_apply.rs:3618) has no direct assertion |
| RestructureTests | applyStalePlanGuard | A stale move (live path != oldPath) is failed, not executed | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::apply_skips_stale_move_when_source_mismatches_db |
| RestructureTests | applyRejectsDistinctCaseOnlyLivePath | A distinct case-only live path fails the stale-plan guard and nothing is moved or overwritten. | a (closed round 14; was d) | platforms/windows/src/engine/src/pipeline/restructure_case_tests.rs::a_case_only_spelling_of_the_planned_path_follows_the_volumes_case_rules (run on the default case-insensitive volume and on a case-sensitive disk image). Was: none: restructure_apply.rs paths_equal (3735) canonicalizes; no test for two distinct case-only paths on a case-sensitive volume (Swift test self-skips on case-insensitive volumes) |
| RestructureTests | applySwapGuard | A same-path file swap (file_ref mismatch) is failed, not executed | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::apply_skips_move_when_file_ref_swapped |
| RestructureTests | fileRefSwappedPredicate | A file-ref swap is flagged only when both refs are known and differ. | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::restructure_identity_fails_closed_and_preserves_high_bit_refs, ::apply_adopts_missing_file_ref_only_for_a_matching_planned_identity |
| RestructureTests | caseOnlyPathIdentityPredicate | Case-only path equality requires positive same-file identity. | a (closed round 14; was d) | platforms/windows/src/engine/src/pipeline/restructure_case_tests.rs::a_case_only_spelling_of_the_planned_path_follows_the_volumes_case_rules (outcome level). Was: none: see paths_equal above; no Rust test that case-only paths need positive same-file identity |
| RestructureTests | pathEqualityFollowsVolumeCaseSemantics | Path equality follows the mounted volume's case semantics. | a (closed round 14; was d) | platforms/windows/src/engine/src/pipeline/restructure_case_tests.rs::a_case_only_spelling_of_the_planned_path_follows_the_volumes_case_rules. Was: none: no Rust test of path equality following the mounted volume case semantics (planner scope case rules are tested in platforms/windows/src/engine/src/commands/restructure.rs::planner_scope_is_case_insensitive_and_sibling_prefix_safe, a different predicate) |
| RestructureTests | applyDbFailureNoDoubleCount | A DB update failure after a successful move is counted once and a recovery record is kept. | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::apply_reports_db_path_update_failure_and_keeps_undo |
| RestructureTests | uniqueDestination | UniqueDestination disambiguates on-disk and claimed collisions | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::unique_destination_avoids_disk_and_claimed_collisions, ::unique_destination_detects_case_only_claimed_collision, ::destination_claims_scale_past_ten_thousand_identical_basenames |
| RestructureTests | unavailableUndoJournalFailsClosed | An unavailable undo journal prevents every move | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::unopenable_undo_journal_aborts_apply_before_any_move |
| RestructureTests | undoJournalWriteFailureStopsApply | An undo-journal write failure stops apply before the next move and the earlier move stays undoable. | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::failed_move_rolls_back_its_journal_entry, ::first_failed_move_restores_the_prior_undo_journal (journal-append failure mid-batch: closest, not an exact injection of a write error) |
| RestructureTests | undoLastRoundTrip | Undo last run restores files to their original locations | b | conformance restructure-apply-and-undo-round-trip |
| RestructureTests | undoAfterWipeAndRescan | Undo still restores files after the catalog was wiped and rescanned (new file ids) | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::undo_after_wipe_and_rescan_rebinds_by_moved_path |
| RestructureTests | undoRecoversPostMoveCrash | Undo recovers a crash after move but before DB update | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::undo_restores_a_moved_but_db_update_failed_file |
| RestructureTests | undoReversesDependentMoves | Undo replays dependent moves in reverse order | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::undo_restores_dependent_moves_in_reverse_order |
| RestructureTests | applyWritesTheCanonicalJournal | Apply writes the shared journal: root header, then entries with source identity. | b | conformance cross-restructure-swift-apply-rust-undo, cross-restructure-rust-apply-swift-undo (journal header + source identity read by Rust: platforms/windows/src/engine/src/pipeline/restructure_apply.rs::undo_reads_a_headerless_swift_journal_inside_the_root_only) |
| RestructureTests | undoRefusesAnotherLibrarysJournal | Undo refuses a journal whose header names another library | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::undo_rejects_a_journal_from_another_library_root; conformance restructure-undo-refuses-another-librarys-journal |
| RestructureTests | undoCancelPreservesJournal | A cancelled undo preserves the journal for a re-run | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::cancelled_undo_reports_retryable_incomplete_result, ::cancel_arriving_after_completed_undo_does_not_preserve_a_stale_journal |
| RestructureTests | partialUndoCanResume | A partially completed undo is idempotent when retried | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::undo_retry_treats_an_already_restored_entry_as_idempotent |
| RestructureTests | largePlannerIsDiskBacked | Large-library planning uses an on-disk plan and bounded preview | a | platforms/windows/src/engine/src/commands/restructure.rs::million_file_large_plan_is_disk_backed_and_preview_bounded |
| RestructureTests | plannersOmitNoOpMoves | Small and disk-backed planners omit moves already at their destination | a | platforms/windows/src/engine/src/commands/restructure.rs::large_plan_counts_and_spool_exclude_in_place_intents, ::stored_plan_writer_never_previews_or_persists_noops (the small in-memory planner has no separate Rust twin) |
| RestructureTests | plannersRespectCaseOnlyPathIdentity | Planners preserve the mounted volume's case-only path semantics (alias is a no-op, distinct case is a rename). | d | none: Swift small and disk planners both honor volume case-only identity; Rust has no test that a case-only destination on a case-insensitive volume is dropped as a no-op and on a case-sensitive volume is proposed |
| RestructureTests | storedPlanPreviewIsBounded | Stored restructure plans expose a bounded preview | a | platforms/windows/src/engine/src/commands/restructure.rs::stored_plan_preview_is_bounded_and_root_bound |
| RestructureTests | applyStoredPlanReadsRustSpool | ApplyStoredPlan reads a spool written by the shared engine (no confidenceCounts in the header) | c | Swift reader for the Rust spool; Rust writes and reads its own (platforms/windows/src/engine/src/commands/restructure.rs::stored_plan_reader_rejects_legacy_v1_with_replan_guidance) |
| RestructureTests | applyStoredPlanFiltersToAutoTier | ApplyStoredPlan applies only the Auto tier; Review/Ask/unknown stay put | a | platforms/windows/src/engine/src/commands/restructure.rs::stored_plan_bulk_apply_is_auto_only, ::stale_auto_tier_row_is_skipped_not_batch_fatal |

### RestructureSemanticTests (15)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| RestructureSemanticTests | distinctiveNaming | Distinctive naming drops ubiquitous tags | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::distinctive_naming_drops_ubiquitous_tags |
| RestructureSemanticTests | tightFolderMatch | Tight match to an existing folder auto-files with a reason | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::tight_match_to_existing_folder_auto_files_with_reason, ::cluster_routes_to_matching_existing_folder |
| RestructureSemanticTests | twoGroupsSeparate | Two distinct content groups get two distinct categories | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::two_content_groups_separate_and_get_distinct_categories |
| RestructureSemanticTests | sanitizationCollidingGroups | Sanitization-colliding group names get distinct folders | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::sanitization_colliding_group_names_get_distinct_folders |
| RestructureSemanticTests | prototypeOutsideRootIgnored | A folder prototype outside the library root is never a routing target; destinations stay inside the root. | a (closed round 14; was d) | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::tests::a_folder_outside_the_library_root_is_never_a_routing_target. Was: none: restructure_semantic.rs:407 guards proto.path.starts_with(library_root) but no test places a prototype outside the root; apply-side backstop is restructure_apply.rs::ensure_inside_root_rejects_traversal and conformance restructure-apply-refuses-a-destination-outside-the-root |
| RestructureSemanticTests | folderPrototypeNameTokens | FolderPrototypes collect folder + sibling filename tokens | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::folder_prototypes_collect_name_tokens, ::folder_prototypes_skip_sparse_folders |
| RestructureSemanticTests | nameAgreementUpgradesToAuto | Filename agreement upgrades a thin content match to Auto | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::name_agreement_upgrades_a_thin_content_match_to_auto |
| RestructureSemanticTests | thinMatchWithoutNamesStaysReview | Thin content match without filename signal stays Review | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::thin_content_match_without_name_agreement_stays_review |
| RestructureSemanticTests | wellPlacedMembersStayPut | Well-placed members stay put instead of reshuffling | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::well_placed_members_stay_put_instead_of_reshuffling |
| RestructureSemanticTests | misplacedMembersRoute | Misplaced members still route to the matching folder | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::misplaced_members_still_route_to_the_matching_folder |
| RestructureSemanticTests | taglessClusterNamedByMonth | A tagless dated cluster is named by month and year, not Unsorted | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::tagless_dated_cluster_is_named_by_month_year_not_unsorted |
| RestructureSemanticTests | labelledTreeMovesOnlyStrays | Labelled tree: only the strays move | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::labelled_tree_moves_only_the_strays |
| RestructureSemanticTests | filenameTokenization | FilenameTokens keeps content words, drops numeric/generic/short | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::filename_tokens_keep_content_drop_generic |
| RestructureSemanticTests | nonImageGroupsByFilename | Non-image pass groups files by filename content | a | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::non_image_pass_groups_by_filename_content |
| RestructureSemanticTests | realDataNonImage | Real-data non-image grouping gives plausible groups (opt-in). | c | Swift opt-in real-data test (skipped without a corpus); not a regression pin |

### RestructureRespectTests (9)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| RestructureRespectTests | genericAndCameraFolderNamesAreRecognised | Generic and camera folder names are recognised | a | platforms/windows/src/engine/src/pipeline/restructure_respect.rs::generic_names_and_roles, ::generic_names_need_exact_numbered_form |
| RestructureRespectTests | formatRolesFromFolderNamesAndFiles | Format roles from folder names and files | a | platforms/windows/src/engine/src/pipeline/restructure_respect.rs::generic_names_and_roles |
| RestructureRespectTests | organizedFilesStayAndEventsNeverMix | Organized files stay and events never mix | a | platforms/windows/src/engine/src/pipeline/restructure_respect.rs::organized_files_stay_and_events_never_mix |
| RestructureRespectTests | looseFilesAreOrganizedButNotIntoWrongFormatsOrJunkNames | Loose files are organized but not into wrong formats or junk names | a | platforms/windows/src/engine/src/pipeline/restructure_respect.rs::loose_files_organized_but_not_into_wrong_formats_or_junk_names |
| RestructureRespectTests | misfiledFormatsMoveToTheirSiblingFolder | Misfiled formats move to their sibling folder | a | platforms/windows/src/engine/src/pipeline/restructure_respect.rs::misfiled_formats_move_to_sibling; conformance restructure-respect-stage |
| RestructureRespectTests | looseFilesJoinTheEventTheyWereCapturedDuring | Loose files join the event they were captured during | a | platforms/windows/src/engine/src/pipeline/restructure_respect.rs::loose_files_join_their_event |
| RestructureRespectTests | cameraFilenamesAndTagIdentifiersAreNotFolderNames | Camera filenames and tag identifiers are not folder names | a | platforms/windows/src/engine/src/pipeline/restructure_respect.rs::camera_names_and_tag_ids_are_not_folder_names |
| RestructureRespectTests | loosePhotosGroupIntoDatedEventsByCaptureGap | Loose photos group into dated events by capture gap | a | platforms/windows/src/engine/src/pipeline/restructure_respect.rs::loose_photos_group_into_dated_events |
| RestructureRespectTests | multiDayEventGetsARangeName | Multi day event gets a range name | a | platforms/windows/src/engine/src/pipeline/restructure_respect.rs::loose_photos_group_into_dated_events (multi-day "2026-09-19 to 09-20" assertion) |

### RestructureRespectPlanTests (4)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| RestructureRespectPlanTests | looseEventsAndFormatCorrectionsSurviveTheAnchorStrip | Loose-event moves and format corrections from the respect stage survive the anchor strip. | a | platforms/windows/src/engine/src/commands/restructure.rs::respect_moves_survive_the_plan_and_their_folders_are_not_kept; conformance restructure-respect-stage |
| RestructureRespectPlanTests | libraryRootIsNeverAnAnchor | The library root is never classified as an Anchor folder. | a | platforms/windows/src/engine/src/commands/restructure.rs::selected_library_root_is_never_a_large_plan_anchor, platforms/windows/src/engine/src/pipeline/restructure.rs::homogeneous_files_directly_under_library_root_are_never_anchor_stripped |
| RestructureRespectPlanTests | matchesReferenceForSeveralK | Exact nearest neighbours match a brute-force reference for several k. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/restructure_semantic.rs::tests::the_k_nearest_selection_matches_a_full_sort_for_several_k (the selection was extracted as `top_k_neighbours`; k in {1, 5, n-1} against a full sort) |
| RestructureRespectPlanTests | rejectsRaggedAndTinyInputs | Ragged or tiny neighbour inputs are rejected. | c | Swift exactNeighbors input validation (nil on ragged/tiny); Rust takes typed fixed-dim vectors |

### RestructureFeedbackTests (4)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| RestructureFeedbackTests | recordThenBoostUpgrades | Record then boost upgrades a matching move to Auto | a | platforms/windows/src/engine/src/pipeline/restructure_feedback.rs::record_then_boost_upgrades_a_matching_move |
| RestructureFeedbackTests | boostNeverUpgradesAsk | Boost never upgrades an Ask move | a | platforms/windows/src/engine/src/pipeline/restructure_feedback.rs::boost_never_upgrades_an_ask_tier_move |
| RestructureFeedbackTests | boostLeavesUnrelatedAlone | Boost leaves an unrelated move alone | a | platforms/windows/src/engine/src/pipeline/restructure_feedback.rs::boost_leaves_unrelated_moves_alone |
| RestructureFeedbackTests | recordAccumulatesWeight | Record accumulates weight on the same token→folder | a | platforms/windows/src/engine/src/pipeline/restructure_feedback.rs::record_accumulates_weight |

### RestructureApplyCancelTests (1)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| RestructureApplyCancelTests | cadence | Apply progress emits on first, last and every interval-th move only. | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::should_emit_apply_progress_cadence |

### MigrationParityTests (4)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| MigrationParityTests | identifiersMatchCanonicalList | Registered migration identifiers match the canonical cross-platform list | a | platforms/windows/src/engine/src/db/migrations.rs::migration_identifiers_match_canonical_list |
| MigrationParityTests | vlmFullModelMigrationIsNullableAndPreservesProvenance | v20 adds a nullable TEXT full-model marker without rewriting provenance. | a | platforms/windows/src/engine/src/db/migrations.rs::fresh_database_applies_all_migrations (column shape asserted; the "provenance not rewritten" upgrade-from-v19 half is not) |
| MigrationParityTests | verdictStableKeySurvivesFaceChurn | V17 stable verdict key survives a face_print id churn | a | platforms/windows/src/engine/src/db/migrations.rs::verdict_stable_key_survives_face_print_churn |
| MigrationParityTests | newerDatabaseRefusesToOpen | DB migrated beyond this engine's registry refuses to open | a | platforms/windows/src/engine/src/db/migrations.rs::newer_db_with_unknown_migration_is_refused, platforms/windows/src/engine/src/db/mod.rs::a_newer_schema_is_refused_without_touching_the_database |

### DatabaseWriterGuardTests (2)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| DatabaseWriterGuardTests | uncancellableWriteSurvivesCancellation | A terminal write issued from a cancelled task still lands. | c | GRDB 7 cancellation behavior (writeUncancellable); rusqlite writes are not cancelled with the task |
| DatabaseWriterGuardTests | prepareDatabasePragmas | Pooled connections run WAL with synchronous=NORMAL and temp_store=MEMORY. | a (closed round 16; was d) | platforms/windows/src/engine/src/db/mod.rs::tests::the_writer_and_a_read_connection_run_with_the_documented_pragmas (WAL, NORMAL, MEMORY, foreign keys on the writer; WAL, MEMORY, query_only on a reader) |

### DBWriterClipBackfillTests (7)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| DBWriterClipBackfillTests | predicateExcludesEmbeddinglessImages | The skip set excludes only embeddable images that lack an embedding. | a (closed round 16; was d) | platforms/windows/src/engine/src/scan_session.rs::tests::image_clip_gate_keeps_embeddingless_images_out_of_the_skip_set_only |
| DBWriterClipBackfillTests | predicateExcludesEmbeddinglessObj | Model skip-set predicate excludes only an embeddingless .obj (no re-walk for non-obj) | a | platforms/windows/src/engine/src/scan_session.rs::model_clip_gate_reprocesses_embeddingless_obj_only |
| DBWriterClipBackfillTests | predicateStopsRewalkingTextlessDocs | Text skip-set predicate stops re-walking a text-less doc (text_stage_done=1) | a | platforms/windows/src/engine/src/scan_session.rs::text_embed_gate_reprocesses_embeddingless_docs_only |
| DBWriterClipBackfillTests | unchangedBackfillFillsThenIsIdempotent | Rescanning an unchanged file fills a missing embedding once and never duplicates it. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/dbwriter.rs::rescan_preservation_tests::rescanning_an_unchanged_image_fills_a_missing_vector_once_and_never_duplicates_it (the idempotence half is not broken by any single guard: the upsert has a unique key) |
| DBWriterClipBackfillTests | legacyRefresh | Legacy image and video vectors are refreshed without changing user tags. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/dbwriter.rs::rescan_preservation_tests::a_vector_from_an_older_model_is_replaced_in_place_and_the_users_tags_stay |
| DBWriterClipBackfillTests | failedLegacyModelRefresh | A failed legacy model render does not repeat indefinitely. | a | platforms/windows/src/engine/src/scan_session.rs::model_clip_gate_reprocesses_embeddingless_obj_only (text_stage_done=0 clause bounds the re-walk of a failed render) |
| DBWriterClipBackfillTests | rejectsMalformedVectors | Malformed vectors are not stored with a verified model identity. | a | platforms/windows/src/engine/src/models/clip_space.rs::exact_space_rejects_legacy_dimensions_and_nonfinite_values (dbwriter.rs:585 ensures valid() before persisting) |

### DBWriterModifiedTimeTests (2)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| DBWriterModifiedTimeTests | keepsStoredNumber | A time a hair apart keeps the stored number and the reviewed chapter current; a real change updates and stales it. | b | conformance scan-keeps-the-modified-times-and-reviewed-rows-of-a-catalog-the-swift-engine-wrote-before; platforms/windows/src/engine/src/pipeline/dbwriter.rs::a_time_a_bit_apart_keeps_the_stored_number_and_a_real_change_updates_it |
| DBWriterModifiedTimeTests | discoveryTime | Discovery reads the modified time from one stat as seconds plus nanoseconds. | c | Swift Discovery single-stat sec+nsec read; Rust discovery.rs reads metadata once by construction (no test) |

### DBWriterRenameHealTests (3)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| DBWriterRenameHealTests | movedFileHealsRow | A moved file (old path gone) re-binds the existing row, preserving id + tags + faces(person_id) | a | platforms/windows/src/engine/src/pipeline/dbwriter.rs::genuine_move_heals_and_preserves_fks, ::file_ref_rename_with_old_path_gone_heals |
| DBWriterRenameHealTests | coexistingHardlinksStayDistinct | Two coexisting hardlinks (same inode, both paths present) stay TWO rows | a | platforms/windows/src/engine/src/pipeline/dbwriter.rs::coexisting_byte_identical_copies_stay_distinct_rows, ::file_ref_collision_with_both_paths_present_stays_distinct |
| DBWriterRenameHealTests | reusedInodeDifferentSizeDoesNotHeal | Inode reuse with a DIFFERENT size does NOT false-heal (size corroboration) | a | platforms/windows/src/engine/src/pipeline/dbwriter.rs::file_ref_heal_requires_matching_size |

### DBWriterStageGateTests (3)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| DBWriterStageGateTests | stageGatesPreserveOnTimeout | A swallowed analysis timeout on a changed file does not wipe tags, face person assignments or OCR text. | a (closed round 14; was d) | platforms/windows/src/engine/src/pipeline/dbwriter.rs::rescan_preservation_tests::a_changed_file_whose_stages_did_not_run_keeps_its_tags_people_and_text. Was: none: Swift Vision/OCR timeout path has no Rust twin, but the product invariant (a rescan that fails a stage keeps tags, faces.person_id and OCR) is untested: dbwriter tests never set face_prints.person_id; stage_skipped_rescan_preserves_prior_metadata covers phash/GPS/camera/flags only |
| DBWriterStageGateTests | timeoutFileIsRetriedNotStranded | A timed-out file is marked failed so an identical-mtime rescan retries it. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/dbwriter.rs::rescan_preservation_tests::a_file_that_timed_out_is_walked_again_even_though_its_date_is_unchanged |
| DBWriterStageGateTests | upsertPreservesMetadataOnStageSkip | A stage-skipped rescan preserves prior phash / GPS / camera / has_faces | a | platforms/windows/src/engine/src/pipeline/dbwriter.rs::stage_skipped_rescan_preserves_prior_metadata |

### DBWriterUpsertTests (3)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| DBWriterUpsertTests | upsertPreservesIdentity | An unchanged rescan keeps files.id and face person assignments; a changed file refreshes its children. | a (closed round 14; was d) | platforms/windows/src/engine/src/pipeline/dbwriter.rs::rescan_preservation_tests::a_rewrite_keeps_the_row_id_and_the_people_the_user_named (an unchanged rewrite; a changed file whose face stage ran deletes and re-inserts its faces by design, so that half of the Swift test is not an invariant of the Rust writer). Was: none: dbwriter.rs deletes face_prints for a reprocessed file (line ~354) and relies on later face passes to reassign; no test asserts files.id is stable or that person_id survives an unchanged re-scan (conformance scan-corrects-stale-person-file-counts covers counts only) |
| DBWriterUpsertTests | pathSearchStoresNFC | insertOne stores the NFC form in path_search. | a | platforms/windows/src/engine/src/pipeline/dbwriter.rs::nfd_filename_is_searchable_by_nfc_query, ::writers_populate_path_search; conformance scan-tiny-tree-catalogs-every-file |
| DBWriterUpsertTests | failedRescanPreservesChildren | A failed rescan records the failure but preserves the prior children. | a (closed round 14; was d) | platforms/windows/src/engine/src/pipeline/dbwriter.rs::rescan_preservation_tests::a_failed_read_of_a_changed_file_records_the_failure_and_keeps_what_was_found_before. Was: none: failed-file path (dbwriter.rs ~3290 failure memory, scan_catchup) is tested for counts, not that a failed rescan keeps the prior tags/faces children |

### ScanRootGuardTests (3)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| ScanRootGuardTests | missingAndEmptyRootsAreUnreachable | Missing and empty roots are unreachable | a | platforms/windows/src/engine/src/scan_session.rs::a_scan_root_must_exist_be_a_folder_and_list_something |
| ScanRootGuardTests | populatedRootIsReachable | Populated root is reachable | a | platforms/windows/src/engine/src/scan_session.rs::a_scan_root_must_exist_be_a_folder_and_list_something; conformance scan-of-an-emptied-root-keeps-the-library-visible |
| ScanRootGuardTests | plainFileIsNotARoot | Plain file is not a root | a | platforms/windows/src/engine/src/scan_session.rs::a_scan_root_must_exist_be_a_folder_and_list_something (a plain file is not trustworthy) |

### FileSystemChangesTests (1)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| FileSystemChangesTests | reportsChangesUnderFolder | FSEvents history reports visible changes under the watched folder only. | c | FSEvents follow-up pass deleted by design (Swift audit B11/B13); Rust compares directory mtimes in scan_session.rs (dirs_changed_since, untested here) |

### LibraryRootsTests (7)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| LibraryRootsTests | countsFilesUnderEachRootAndNotLookAlikeSiblings | Counts files under each root and not look alike siblings | a | platforms/windows/src/engine/src/commands/roots.rs::counts_files_under_each_root_and_not_in_lookalike_siblings |
| LibraryRootsTests | aNestedLocationCountsItsOwnFilesAndItsParentDoesNot | A nested location counts its own files and its parent does not | a | platforms/windows/src/engine/src/commands/roots.rs::a_nested_location_counts_its_own_files_and_its_parent_does_not |
| LibraryRootsTests | aMissingOrEmptyFolderIsOfflineAndAPopulatedOneOnline | A missing or empty folder is offline and a populated one online | a | platforms/windows/src/engine/src/commands/roots.rs::a_missing_or_empty_folder_is_offline_and_a_populated_one_online |
| LibraryRootsTests | boundsHandleTrailingSeparatorsAndWindowsPaths | Bounds handle trailing separators and windows paths | a | platforms/windows/src/engine/src/commands/roots.rs::windows_style_roots_use_backslash_bounds, platforms/windows/src/engine/src/scan_session.rs::prefix_upper_bound_brackets_the_prefix |
| LibraryRootsTests | aScopeKeepsItsOwnFilesAndLeavesLookalikesAndDeeperLocationsOut | A scope keeps its own files and leaves lookalikes and deeper locations out | a | platforms/windows/src/engine/src/commands/roots.rs::a_scope_keeps_its_own_files_and_leaves_lookalikes_and_deeper_locations_out |
| LibraryRootsTests | scopedSearchKeepsOneLocationAndUnknownLocationsAreErrors | Scoped search keeps one location and unknown locations are errors | a | platforms/windows/src/engine/src/commands/roots.rs::the_sql_clause_agrees_with_contains_and_unknown_ids_are_errors; conformance search-root-scope-keeps-one-location |
| LibraryRootsTests | rootsListRoundTripsThroughTheWireFormat | Roots list round trips through the wire format | c | Swift Codable round trip of RootsList; wire shape is pinned by shared/ipc-schema (schema drift check) and Rust ipc/generated.rs |

### SecurityScopedAccessRegistryTests (3)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| SecurityScopedAccessRegistryTests | bookmarkPathMustMatch | Bookmark path must match requested folder | b | conformance folder-access-bookmarks-grant-and-refuse-like-the-swift-engine (another folder's bookmark refused); platforms/windows/src/engine/src/commands/folder_access.rs::a_bookmark_that_does_not_resolve_is_refused |
| SecurityScopedAccessRegistryTests | malformedBookmarkIsRejected | Malformed bookmarks are rejected | b | conformance folder-access-bookmarks-grant-and-refuse-like-the-swift-engine; platforms/windows/src/engine/src/commands/folder_access.rs::a_bookmark_that_does_not_resolve_is_refused |
| SecurityScopedAccessRegistryTests | rootAndOperationLeasesBalance | Root and operation leases balance security scope access | c | Swift registry lease counting; the surviving equivalent is BookmarkLeases in platforms/apple/Tests/AppleKitTests/AppleKitTests.swift (count==0 after release; shutdownReleasesEverything), not Rust |

### DispatchHandlersTests (7)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| DispatchHandlersTests | databaseOpenErrorIsGeneric | Database-open errors shown to the UI never expose local paths. | a | platforms/windows/src/engine/src/server.rs::database_open_errors_name_their_cause (exact text for newer-db; others starts_with/contains, no negative path-leak assertion) |
| DispatchHandlersTests | startScanDbUnavailableEmitsTerminal | startScan with no database emits a terminal scanComplete, not just an error. | a (closed round 16; was d) | platforms/windows/src/engine/src/server_fuzz_tests.rs::start_scan_with_no_database_answers_db_unavailable_then_a_terminal_phase (Rust ends with PhaseChanged(Failed), not scanComplete; the app treats both as terminal) |
| DispatchHandlersTests | restructureRoundTrip | PlanRestructure / applyRestructure round-trip through the engine butler | b | conformance restructure-apply-and-undo-round-trip, restructure-messy-downloads-of-documents |
| DispatchHandlersTests | undoRestructureShortcutTokenFailsClosed | UndoRestructure with a shortcutUndoToken fails closed and doesn't wedge the reservation | a | platforms/windows/src/engine/src/pipeline/restructure_apply.rs::shortcut_undo_unknown_token_is_not_ambiguous_success, ::shortcut_undo_retry_returns_the_durable_completion_receipt; platforms/windows/src/engine/src/server.rs::an_organize_folders_move_frees_its_slot_with_its_result; conformance restructure-shortcut-apply-and-undo |
| DispatchHandlersTests | restructurePlanDTOMapping | RestructurePlan DTO maps proposals and rolls up category counts | b | conformance restructure-messy-downloads-of-documents (moves, destination, category); platforms/windows/src/engine/src/pipeline/restructure.rs::category_counts_summed_and_sorted |
| DispatchHandlersTests | restructurePlanConfidenceCounts | The plan publishes complete confidence totals that sum to the move count. | a* (closed round 16; was d) | platforms/windows/src/engine/src/commands/restructure_plan_counts_tests.rs (counts equal the tally of the listed moves and sum to the total; the fixture yields only auto moves, so the review and ask columns are not individually pinned) |
| DispatchHandlersTests | exemptAnchorFolderBecomesMixed | FolderTiersAndCounts remaps an exempt (semantic-claimed) Anchor folder to Mixed | a | platforms/windows/src/engine/src/pipeline/restructure.rs::semantic_exemption_is_scoped_to_the_claimed_file; conformance restructure-anchor-strip-exempts-only-claimed-files |

### ScanCancellationTests (5)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| ScanCancellationTests | cancelMidScanCompletes | Cancelling mid-scan still yields scanComplete and the engine stays responsive and shuts down cleanly. | a (closed round 16; was d) | platforms/windows/src/engine/src/commands/scan_cancel_tests.rs::a_scan_cancelled_part_way_ends_with_cancelled_and_scan_complete_and_frees_its_slot |
| ScanCancellationTests | scanRootMustBeAbsoluteWithoutNUL | A scan root with a NUL, or one that is not absolute, is refused instead of scanning the working directory | a | platforms/windows/src/engine/src/commands/scan.rs::a_scan_root_must_be_absolute_and_free_of_nul; conformance scan-refuses-a-root-that-is-not-an-absolute-path |
| ScanCancellationTests | operationSpecificCancellation | Scan and restructure cancellation stay isolated across later jobs | a | platforms/windows/src/engine/src/server.rs::scan_and_restructure_cancel_signals_are_isolated, ::a_fresh_restructure_operation_is_not_poisoned_by_an_old_cancel |
| ScanCancellationTests | restructureReservationIsExclusive | Only one restructure operation can be reserved at a time. | a (closed round 16; was d) | platforms/windows/src/engine/src/server_fuzz_tests.rs::a_second_organize_folders_move_is_refused_while_the_first_runs_and_the_first_completes |
| ScanCancellationTests | periodicProgressStopsAtTerminalPhase | Periodic progress stops after a terminal scan phase. | b | conformance scan-ends-with-a-progress-snapshot-of-the-terminal-phase; platforms/windows/src/engine/src/scan_session.rs::the_last_snapshot_carries_the_terminal_phase_and_the_final_counts |

### EngineQueryRepliesTests (26)

| Suite | Test | Invariant | Cat | Rust pin / reason |
|---|---|---|---|---|
| EngineQueryRepliesTests | mergeSuggestionsReachEveryWaitingCaller | Merge suggestions reach every waiting caller | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (EngineQueryReplies waiter) |
| EngineQueryRepliesTests | mergeSuggestionFailureEndsTheWaitButOtherErrorsDoNot | A merge-suggestion failure ends the wait but other errors do not | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (EngineQueryReplies waiter) |
| EngineQueryRepliesTests | textEmbeddingsAreMatchedByQueryID | Text embeddings are matched by query id | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (EngineQueryReplies waiter) |
| EngineQueryRepliesTests | anEngineWithoutTheTextModelLeavesSearchOnKeywords | An engine without the text model leaves search on keywords | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (EngineQueryReplies waiter) |
| EngineQueryRepliesTests | videoThumbnailsAreMatchedByPathAndRevision | Video thumbnails are matched by path and revision | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (EngineQueryReplies waiter) |
| EngineQueryRepliesTests | eachThumbnailSizeGetsItsOwnFrame | Each thumbnail size gets its own frame | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (EngineQueryReplies waiter) |
| EngineQueryRepliesTests | aSourceSmallerThanEveryRequestAnswersThemAll | A source smaller than every request answers them all | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (EngineQueryReplies waiter) |
| EngineQueryRepliesTests | aCommandThatNeverLeftTheAppReturnsAtOnce | A command that never left the app returns at once | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (waiter) |
| EngineQueryRepliesTests | silenceTimesOutWithoutLeavingAWaiter | Silence times out without leaving a waiter | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (waiter) |
| EngineQueryRepliesTests | cancellingTheCallerReleasesItsWaiter | Cancelling the caller releases its waiter | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (waiter) |
| EngineQueryRepliesTests | anAlreadyCancelledCallerNeverWaits | An already-cancelled caller never waits | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (waiter) |
| EngineQueryRepliesTests | anEngineExitReleasesEveryWaiter | An engine exit releases every waiter | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (waiter) |
| EngineQueryRepliesTests | aFrameDecodedOffTheMainActorReachesItsWaiter | A frame decoded off the main actor reaches its waiter | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (waiter) |
| EngineQueryRepliesTests | aModelLoadBarEndsWhenDescribingStartsOrFinishes | A model-load bar ends when describing starts or finishes | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (model-load bar) |
| EngineQueryRepliesTests | aCatalogReplyResumesItsWaiterWithoutPolling | A catalog reply resumes its waiter without polling | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (catalog waiter) |
| EngineQueryRepliesTests | aCatalogWaitEndsOnTimeoutCancellationOrAnUnsentRequest | A catalog wait ends on timeout, cancellation or an unsent request | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (catalog waiter) |
| EngineQueryRepliesTests | aBulkResultWaitTakesTheFirstMatchingResultAfterItsSignal | A bulk-result wait takes the first matching result after its signal | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (bulk-result waiter) |
| EngineQueryRepliesTests | anEngineClientWithNoEngineAnswersNilAtOnce | An EngineClient with no engine answers nil at once | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (EngineClient) |
| EngineQueryRepliesTests | theEngineIsAskedForTheBucketsPixelSize | The engine is asked for the bucket's pixel size | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (ThumbnailService) |
| EngineQueryRepliesTests | onlyVideoExtensionsAskTheEngine | Only video extensions ask the engine | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (ThumbnailService) |
| EngineQueryRepliesTests | anEngineFrameSkipsQuickLook | An engine frame skips QuickLook | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (ThumbnailService) |
| EngineQueryRepliesTests | aSilentOrRefusingEngineFallsBackToQuickLook | A silent or refusing engine falls back to QuickLook | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (ThumbnailService) |
| EngineQueryRepliesTests | noEngineMeansQuickLook | No engine means QuickLook | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (ThumbnailService) |
| EngineQueryRepliesTests | anEngineThatIsNotRunningFallsBackToQuickLook | An engine that is not running falls back to QuickLook | c | App-side EngineQueryReplies/EngineClient code; survives retirement (not engine code), needs no engine (ThumbnailService) |
| EngineQueryRepliesTests | None | A real engine's merge-suggestion, text-embedding and video-thumbnail replies reach the waiting caller (empty list, nil without CLIP, requested long side, an empty-bytes reply when undecodable). | b | conformance queries.json: people-merge-suggestions-empty-library, search-embed-text-without-clip, files-video-thumbnail-undecodable-answers-empty; platforms/windows/src/engine/src/commands/thumbnail.rs::requested_size_defaults_and_clamps, ::larger_requests_are_honoured_but_never_upscale (192/512/1024 long side) |
| EngineQueryRepliesTests | noAppSourceBuildsATextEncoderSession | No app source builds a text-encoder session; the engine owns the CLIP text model. | c | App source-grep policy (no CLIPTextEncoder/ORTSession in app sources); survives retirement, not engine code |

### Gaps (d), ranked

Cost of regression, data loss and wrong file operations first, then wrong results, then presentation. Rust paths are under `platforms/windows/src/engine/src/`.

| # | Swift test | Cost of regression | Suggested Rust module and test shape |
|---|---|---|---|
| 1 | DBWriterUpsertTests.upsertPreservesIdentity | Data loss: a rescan could reassign `files.id` or drop `face_prints.person_id`, losing named people and every FK child. | `pipeline/dbwriter.rs` tests: write a file, assign a face to a person, rewrite unchanged (same id, same person_id), then changed (id stable, children refreshed, user tags kept). |
| 2 | DBWriterUpsertTests.failedRescanPreservesChildren | Data loss: a failed re-read of a changed file could wipe its prior tags, faces and text. | `pipeline/dbwriter.rs`: seed tags/faces/OCR, flush a `failed=true` record for the same path, assert children and row id survive and the failure is recorded. |
| 3 | DBWriterStageGateTests.stageGatesPreserveOnTimeout | Data loss: a stage that times out on a changed file could wipe user-visible tags, `person_id` and OCR. | `pipeline/dbwriter.rs`: same shape as 2 with a stage-not-run flag set (extend `stage_skipped_rescan_preserves_prior_metadata` to tags, faces.person_id, ocr_text). |
| 4 | CrashSafetyTests.trashJournalsEachFileBeforeTheNext | Data loss: a kill after a file lands in the Trash with no journal entry leaves it unrestorable by the app. | `commands/bulk.rs` tests: inject the trash backend, assert `trash_log::append` ran before the first move (and macOS `bind_macos_trash_receipts` writes the location), then kill-simulate by failing the second move. |
| 5 | CrashSafetyTests.reconcileReadsTheRustLogAfterAKill | Wrong results/data: ghost catalog rows for files already in the Trash (app shows files that are gone; clicks fail). Also the forged-key (wrong HMAC) check is untested. | `commands/trash_log.rs`: call `reconcile_trashed_rows` with a stub `locate`, a signed entry and a foreign-key entry; assert only the signed one drops its row and person counts reconcile. |
| 6 | CrashSafetyTests.restoreKeepsTheJournalUntilEachFileIsBack | Data loss risk: a kill mid-restore must not forget files still in the Trash. Holds by design (append-only log) but unpinned. | `commands/trash.rs`: restore a 2-file batch with the second move failing; assert `find_batch_in` still returns both items and a retry restores the second. |
| 7 | RestructureTests.applyRejectsDistinctCaseOnlyLivePath | Wrong file operation: on a case-sensitive volume a stale plan could move or overwrite a different file (`Report.jpg` vs `report.jpg`). | `pipeline/restructure_apply.rs`: on a case-sensitive temp volume (skip if aliasing), plan `Report.jpg`, swap in `report.jpg`, assert applied=0, failed=1 and both files untouched. |
| 8 | RestructureTests.caseOnlyPathIdentityPredicate | Same as 7: `paths_equal` could treat case-only variants as one file without proof of identity. | `pipeline/restructure_apply.rs`: unit-test `paths_equal` on aliasing and non-aliasing pairs (needs a seam for the identity lookup). |
| 9 | RestructureTests.pathEqualityFollowsVolumeCaseSemantics | Same as 7. | Same module: `paths_equal` on the temp volume, both outcomes asserted by probing which semantics the volume has. |
| 10 | RestructureTests.plannersRespectCaseOnlyPathIdentity | Wrong results/file ops: planner proposes a no-op rename on a case-insensitive volume, or misses a real case rename on a case-sensitive one. | `commands/restructure.rs`: plan one already-sorted file whose name differs only in case from its target; assert no move when the volume aliases and one move when not (small and stored planners). |
| 11 | RestructureSemanticTests.prototypeOutsideRootIgnored | Wrong file operation in the plan: files proposed into a folder outside the library root (apply refuses it, so the move fails late). | `pipeline/restructure_semantic.rs`: classify with a prototype under `/other` and root `/lib`; assert every destination starts with the root. |
| 12 | BulkMutationTests.wipeEmptiesCatalog | Wrong results: a wipe leaving rows in some table or with foreign keys off corrupts the next scan and search. | `db/mod.rs`: seed every table class (files, tags, doc_text, clip/text embeddings), run `wipe_all`, assert all non-meta tables are empty, `PRAGMA foreign_keys=1`, vector state re-seeded at generation 0 then 1 after an insert. |
| 13 | CrashSafetyTests.vanishedFileGetsNoFailedRow | Wrong results: phantom "Unreadable" rows for files deleted mid-scan. | `pipeline/dbwriter.rs`: feed a failed record for a path that does not exist and one that exists; assert only the existing path gets a row. |
| 14 | ScanCancellationTests.cancelMidScanCompletes | Wrong results: the app could wait forever for `scanComplete` after Cancel, or the engine could wedge. | `commands/scan.rs` (tokio test) with a real temp tree: start a scan, cancel after the first batch, assert the event stream ends with `PhaseChanged(Cancelled)` and `ScanComplete` and the slot is free; or a conformance scan case with a cancel step. |
| 15 | DispatchHandlersTests.startScanDbUnavailableEmitsTerminal | Wrong results: with no database the app's scan UI could stay "scanning". | `server.rs`: dispatch `startScan` with `db=None`; assert `db_unavailable` error followed by `ScanComplete`. |
| 16 | DBWriterClipBackfillTests.unchangedBackfillFillsThenIsIdempotent | Wrong results: images scanned before CLIP was installed never get embeddings (search blind), or duplicates. | `pipeline/dbwriter.rs`: write an image with no vector, rewrite unchanged with a vector (one row), rewrite again (still one row). |
| 17 | DBWriterClipBackfillTests.predicateExcludesEmbeddinglessImages | Wrong results: same as 16 via the scan skip set. | `scan_session.rs`: sibling of `video_clip_gate_reprocesses_frameless_videos_only` for `SKIP_SET_IMAGE_CLIP_GATE`. |
| 18 | DBWriterClipBackfillTests.legacyRefresh | Wrong results: legacy-model vectors may stay in a new vector space (verify the Rust intent first; gates carry no model-id clause). | `pipeline/dbwriter.rs`: seed a vector with an old model id plus a user tag, rewrite, assert model updated and tag kept. |
| 19 | DBWriterStageGateTests.timeoutFileIsRetriedNotStranded | Wrong results: a file that timed out in analysis is skipped forever by identical-mtime rescans. | `scan_session.rs`: a failed row with an identical mtime must stay out of the skip set (near `files_that_keep_failing_join_the_skip_set...`). |
| 20 | RestructureTests.mixedHomogeneityDominantPerson | Wrong results: folders with one outlier misclassified, causing wrong anchor strips (moves of organized files). | `pipeline/restructure.rs`: `classify_folders` with 5 Alice + 1 Bob moves, assert Anchor. |
| 21 | RestructureRespectPlanTests.matchesReferenceForSeveralK | Wrong results: exact kNN bug distorts restructure grouping. | `pipeline/restructure_semantic.rs`: 300x48 deterministic vectors, compare the exact kNN to brute force for k in {1,5,n-1}. |
| 22 | ScanCancellationTests.restructureReservationIsExclusive | Wrong file operations (two applies at once): guarded by code, unpinned. | `server.rs`: hold `restructure_mutation`, start a second, assert `restructure_already_running` and the first still completes. |
| 23 | DispatchHandlersTests.restructurePlanConfidenceCounts | Presentation/wrong results: the preview's auto/review/ask counts do not add up. | `commands/restructure.rs`: build a plan with all four tiers, assert counts and that they sum to the move count. |
| 24 | BulkMutationTests.trashIDsAreOrderedUnique | Wrong results: duplicate ids trashed or reported twice. | `commands/bulk.rs`: trash request `[3,1,3,2,1]`, assert 3 results in order 3,1,2. |
| 25 | PersonMergeUndoTests.trashRefusalsCarryAReason | Presentation: a refusal without a reason shows a generic message. | `commands/bulk.rs`: trash an unknown id, assert `reason == "missing"`. |
| 26 | DatabaseWriterGuardTests.prepareDatabasePragmas | Presentation/perf and durability: WAL/NORMAL/foreign_keys silently off on a pooled reader. | `db/mod.rs`: open writer and a pooled reader, read back `journal_mode`, `synchronous`, `temp_store`, `foreign_keys`. |


## Area B: scan, decode, residency, IPC sink, models


Rust paths are relative to platforms/windows/src/engine/src unless they start with 'platforms/', 'shared/' or 'scan.json' (shared/test-corpus/conformance/scan.json). A trailing ~ means the pin was matched by test name and body skim, not proven to fail when the invariant breaks. Names in parentheses are parametrized or display-title-only tests.


### DiscoveryTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| DiscoveryTests | smallTree | A walk returns the sorted, filtered files of a tree. | a | platforms/windows/src/engine/src/pipeline/discovery.rs::synthetic_tree_walk_prunes_noise_and_counts_files; scan.json scan-tiny-tree-catalogs-every-file |
| DiscoveryTests | streamingPublishesEarlyProgress | Streaming discovery reports counts before the walk ends. | a | pipeline/discovery.rs::count_increments_before_channel_send (read round 16: asserts count == 100 with the receiver undrained, so a counter moved to the consumer fails it; progress in the middle of a walk is not asserted) |
| DiscoveryTests | skipsLargeFiles | Documents and images over the size cap are skipped; large streamed video is kept. | d | Rust walk has no size cap (unix_edge_cases_walk_to_completion keeps a 6 GiB mp4; no test of an oversize image/doc). Missing: walk a tree with an over-cap tif/pdf and assert the stated outcome (skip or keep) in discovery.rs Round 16: Rust has no size cap by design (the walk keeps a 6 GiB mp4), so the Swift rule is not a Rust invariant; decide before pinning. |
| DiscoveryTests | discoversTextData | Plain-text data extensions (csv, json, yaml...) are discovered as documents. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/discovery.rs::plain_text_data_and_camera_raw_extensions_classify_as_indexed_kinds. Was: only image, audio and model kinds were asserted |
| DiscoveryTests | prunesExcludedFolders | User-excluded folders are pruned and siblings kept. | a | pipeline/discovery.rs::synthetic_tree_walk_honors_user_exclusions |
| DiscoveryTests | skippedFileScannedAtIsBumped | An unchanged skipped file still gets scanned_at updated so later sweeps treat it as seen. | a | pipeline/discovery.rs::skipped_but_present_files_stay_in_the_seen_set (read round 16: Rust does not bump scanned_at of a skipped row; the seen set keeps the row from being hidden, which is what this test pins. The only scanned_at touch is the heal-only path: pipeline/dbwriter.rs::heal_only_tests) |
| DiscoveryTests | sizeMismatch | A changed size never skips a file. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/discovery.rs::the_skip_decision_follows_size_and_modified_time_over_the_whole_matrix (also classify_entry_builds_full_payload_off_consumer). Failed when the size compare was removed |
| DiscoveryTests | equalMtimeSkips | Equal size and equal mtime skip the file. | a | platforms/windows/src/engine/src/pipeline/discovery.rs::the_skip_decision_follows_size_and_modified_time_over_the_whole_matrix, classify_entry_builds_full_payload_off_consumer (read round 16: the unchanged file is skipped and still seen) |
| DiscoveryTests | backdatedEditNotSkipped | A same-size edit with a different (older) mtime is reprocessed. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/discovery.rs::the_skip_decision_follows_size_and_modified_time_over_the_whole_matrix (a stored time a day older or newer than the disk re-reads). Failed when the time compare was removed |
| DiscoveryTests | bothNilSkips | Both-nil stored/disk mtime counts as unchanged. | a (closed round 16; was d; Rust differs from Swift) | platforms/windows/src/engine/src/pipeline/discovery.rs::the_skip_decision_follows_size_and_modified_time_over_the_whole_matrix pins the Rust rule: a row with no stored time is never current and is re-read (Swift skips it). Rust writes a time for every scanned row, so only an old or hand-edited catalog hits it; the cost is one re-read. Failed when the rule was flipped |
| DiscoveryTests | nilVsPresentNotSkipped | A one-sided nil mtime is a change. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/discovery.rs::the_skip_decision_follows_size_and_modified_time_over_the_whole_matrix (stored time missing, disk time present) |
| DiscoveryTests | tinyDriftSkips | Sub-microsecond mtime drift still skips. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/discovery.rs::the_skip_decision_follows_size_and_modified_time_over_the_whole_matrix (4e-7 s skips, 3e-6 s re-reads). Failed when the tolerance was widened to 1 s |
| DiscoveryTests | commonCameraAndAudioFormatsAreIndexed | Common camera RAW and audio extensions classify as indexed kinds. | a | pipeline/discovery.rs::kind_from_extension_audio_and_model, kind_from_extension_image_set, plain_text_data_and_camera_raw_extensions_classify_as_indexed_kinds (RAW and audio extensions asserted since round 16) |
| DiscoveryTests | symlinkedRootCanonicalizes | A symlinked scan root walks its target with child paths in the same form. | d | unix_edge_cases_walk_to_completion covers symlinks inside the tree, not a symlinked ROOT. Missing: root-is-symlink walk asserting child path prefix |
| DiscoveryTests | shortfall | Unreadable folders and oversize files give one plain notice; a clean walk gives none. | a | scan_session.rs::cloud_only_files_are_reported_in_the_swift_engines_words, an_unreachable_root_keeps_the_notice_for_a_disconnected_drive (read round 16: unreadable and cloud-only items give one notice in the Swift words, a clean walk gives none; the oversize-file half does not exist in Rust, which has no size cap) |

### DocumentAnalysisTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| DocumentAnalysisTests | salientTags | RAKE tags keep the Windows bounds and ordering. | a | platforms/windows/src/engine/src/util/keywords.rs::extracts_a_salient_phrase, scores_are_descending, tag_count_is_capped |
| DocumentAnalysisTests | noiseFiltering | Timestamps and stopwords do not become tags. | a | platforms/windows/src/engine/src/util/keywords.rs::iso_timestamp_does_not_become_a_tag, stopwords_split_phrases |
| DocumentAnalysisTests | punctuationEndsPhrases | Punctuation and line breaks end a phrase. | a | platforms/windows/src/engine/src/util/keywords.rs::punctuation_and_line_breaks_end_phrases |
| DocumentAnalysisTests | longRunsAreSplit | Runs over four words become single keywords. | a | platforms/windows/src/engine/src/util/keywords.rs::runs_over_four_words_become_single_keywords |
| DocumentAnalysisTests | oneLetterTokens | One-letter tokens are not keywords. | a | platforms/windows/src/engine/src/util/keywords.rs::one_letter_tokens_are_not_keywords |
| DocumentAnalysisTests | groundedFilename | Grounded filenames use only extracted words. | a | platforms/windows/src/engine/src/util/keywords.rs::grounded_filename_uses_three_to_five_source_words |
| DocumentAnalysisTests | plainText | Plain text extraction is bounded. | a | pipeline/doc_extract.rs::extract_text_file_passes_through, truncate_respects_char_boundary |
| DocumentAnalysisTests | longTextKeepsUTF8 | Long text files index up to 256 KiB and a cap inside a character does not corrupt it. | a* | pipeline/doc_extract.rs::truncate_handles_cut_inside_multibyte_char (256 KiB value ~ not asserted) Read round 16: the test asserts the cut stays inside the cap and on a character, not the 256 KiB value itself. |
| DocumentAnalysisTests | powerpoint | PowerPoint slide text and entities are extracted. | a | pipeline/doc_extract.rs::xml_text_runs_collects_only_target_elements, xml_text_runs_resolves_entities |
| DocumentAnalysisTests | corruptPresentation | Corrupt presentations fail closed and promptly. | a* | pipeline/doc_extract.rs::archive_entry_preflight_rejects_central_directory_amplification, archive_preflight_rejects_fake_eocd... ~ (promptness not asserted) Read round 16: both tests assert a refusal with a reason; promptness is not asserted. |
| DocumentAnalysisTests | completedExtractionKeepsCircuitOpen | A finished extraction is not invalidated by its stale timeout. | c | Swift DocText timeout circuit; Rust extraction has no equivalent timer (disappears) |
| DocumentAnalysisTests | xmlEntities | OOXML entities decode once. | a | pipeline/doc_extract.rs::xml_text_runs_resolves_entities |
| DocumentAnalysisTests | pdfTextAndRaster | Text-layer PDFs extract text and render a bounded first page. | c | stays, moves to AppleKitTests: ScanDocuments.swift in FileIDAppleKit is the shared implementation (confirm ScanAdapterTests covers PDF text+raster; else gap) |
| DocumentAnalysisTests | scanTagging | Scan tagging persists bounded document keywords and scores. | b | scan.json scan-mac-adapters-store-what-the-swift-scan-stores ~ (keyword bound/score not asserted per row) Read round 16: the case asserts the document keyword tags per file; the keyword count bound and scores are not asserted. |
| DocumentAnalysisTests | corruptPDFTagging | A corrupt PDF stays retryable and keeps prior metadata. | d | dbwriter::stage_skipped_rescan_preserves_prior_metadata covers skipped stages only. Missing: scan a re-written corrupt PDF over an existing row, assert failed/retry and tags kept |
| DocumentAnalysisTests | metadataLessAudio | Audio with no metadata degrades to a stable Audio tag. | d | scan-stores-the-kind-tag... uses tagged mp3 only. Missing: untagged audio -> only 'Audio' |
| DocumentAnalysisTests | classifications | Every promised extension routes to the intended analysis kind. | d | No Rust test of extension->analysis kind for Describe Files (deep_scope.rs not matched). Missing table test in commands/deep_scope.rs |
| DocumentAnalysisTests | prompts | Prompts are type-specific; text-only prompts forbid visual invention. | d | Only document_prompt_quotes_untrusted_instructions_as_json_data exists. Missing: prompt content assertions in pipeline/deep_analyze_naming.rs |
| DocumentAnalysisTests | promptInjectionBoundary | Quoted document text cannot become model instructions. | a | pipeline/deep_analyze_naming.rs::document_prompt_quotes_untrusted_instructions_as_json_data |
| DocumentAnalysisTests | textOnlyGrounding | Text-only results strip unsupported visual claims and ground names. | d | Missing: result post-filter test in pipeline/deep_analyze_naming.rs |
| DocumentAnalysisTests | targetResolution | Target resolution includes documents and rejects unsupported files. | d (moved from a, round 16) | commands/deep_scope.rs tests read round 16: they pin selection, exclusions and path boundaries only. Nothing asserts that documents are targets and unsupported kinds are refused. |

### TextReadingTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TextReadingTests | garbledFastReadsMarkHandwritingButNotPrint | Garbled fast reads flag handwriting, clean print does not. | a | pipeline/text_reading.rs::garbled_reads_flag_handwriting_but_not_print |
| TextReadingTests | carefulReadRunsOnHandwritingShortOrMangledText | The careful read runs for handwriting, short or mangled text. | d | No Rust test of the careful-read trigger rule. Missing: text_reading.rs table test |
| TextReadingTests | handwritingLabelNeedsATagOrAMangledFastRead | The handwriting label needs a tag or a mangled fast read. | d | same gap |
| TextReadingTests | disagreementSeparatesPrintFromHandwriting | Fast/accurate read disagreement separates print from handwriting. | d | same gap |
| TextReadingTests | accurateTierPausesAfterThreeFailuresInARow | The accurate tier pauses after three consecutive failures. | d | No Rust test; dbwriter::three_failures_of_the_same_file_are_remembered is per-file, not a tier breaker |
| TextReadingTests | accurateReadsRunInParallelAndReadPrintedText | Accurate reads run in parallel and read printed text (env gated). | c | stays, FILEID_OCR_TESTS-gated; Vision OCR lives in FileIDImaging/adapters - move to AppleKitTests |
| TextReadingTests | documentShapeMatchesLetterAndA4ButNotPhotos | Letter and A4 aspect match document shape; photos do not. | a | pipeline/text_reading.rs::document_shape_matches_letter_and_a4_but_not_photos |
| TextReadingTests | transcriptionTargetsUnreadTextBearingImagesAndScannedPDFs | Transcription targets unread text-bearing images and scanned PDFs. | a | pipeline/text_reading.rs::transcription_targets_unread_text_bearing_images_and_scanned_pdfs |
| TextReadingTests | parseTranscriptionStripsMarkerAndRejectsNone | Transcription parsing strips the marker and rejects 'none'. | a | pipeline/text_reading.rs::parse_transcription_strips_marker_and_rejects_none |
| TextReadingTests | scanStoresTheHandwritingFlagAndRescanKeepsATranscription | A scan stores the handwriting flag; a rescan keeps a stored transcription. | a | pipeline/text_reading.rs::stored_transcription_survives_rescan_until_the_file_changes (flag storage: dbwriter::vision_faces_and_text_reads_are_stored_as_the_swift_scan_stores_them) |
| TextReadingTests | transcriptionIsSearchableAsHandwritingAndSurvivesRescanUntilTheFileChanges | Transcription is searchable as handwriting and survives rescan until the file changes. | a | pipeline/text_reading.rs::stored_transcription_survives_rescan_until_the_file_changes (read round 16: asserts the transcription is found by an ocr_fts MATCH, and gone after the file changes) |

### ArchiveNamesTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ArchiveNamesTests | listsNames | Zip entry names are listed; folders and hidden entries are not. | a | pipeline/archive_names.rs::lists_names_and_survives_corruption |
| ArchiveNamesTests | listsZip64 | ZIP64 archives are listed. | a | pipeline/archive_names.rs::lists_zip64_archives |
| ArchiveNamesTests | survivesCorruption | Corrupt or truncated zips return nil without crashing. | a | pipeline/archive_names.rs::lists_names_and_survives_corruption |

### ContentHashTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ContentHashTests | identicalEqual | Identical bytes hash equal regardless of path. | a | util/content_hash.rs::identical_content_hashes_equal_regardless_of_path |
| ContentHashTests | differentDiffer | Different bytes hash differently. | a | util/content_hash.rs::different_content_hashes_differ |
| ContentHashTests | compositeDeterministic | Composite hash is deterministic and differs from the full hash. | a | util/content_hash.rs::composite_path_is_deterministic_and_differs_from_full |
| ContentHashTests | compositeEdge | Composite catches a differing edge byte. | a | util/content_hash.rs::composite_hash_notices_a_change_in_only_the_first_or_only_the_last_edge_byte (round 16: the older composite_detects_a_changed_middle_byte_via_size_or_edges changes both edges against each other, on a file smaller than one chunk, so it could not tell the edges apart). Failed when the tail read was removed |

### CaptureDateBackfillTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CaptureDateBackfillTests | backfillsOnce | Copy dates on photos are replaced by EXIF times once per folder. | a | pipeline/capture_date_backfill.rs::copy_dates_on_photos_are_replaced_by_exif_times_once_per_folder |

### ExifCaptureDateTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ExifCaptureDateTests | readsOffsetsAndRejectsPlaceholders | EXIF time offsets are honoured and placeholder dates rejected. | a | pipeline/tagging.rs::exif_datetime_reads_offsets_and_rejects_placeholders |

### ScanCatchUpTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ScanCatchUpTests | modeSwitch | RAM++ is deferred when loaded; FILEID_RAMPLUS_INLINE=1 keeps it in the scan. | a | pipeline/catch_up.rs::ram_plus_is_deferred_by_default_on_the_mac_and_on_request_elsewhere |
| ScanCatchUpTests | visionContextRoundTrip | The stored classifier context yields the same combined tags as full output. | a | pipeline/catch_up.rs::the_context_reads_and_writes_the_swift_engines_json, the_context_keeps_exactly_what_the_tag_agreement_reads |
| ScanCatchUpTests | writerSetsAndClearsMarker | The writer records what a file owes with its row and clears it on full re-analysis. | a | pipeline/dbwriter.rs::work_owed_is_stored_with_the_row_and_replaced_by_a_full_analysis |
| ScanCatchUpTests | hiddenRowOwesNothing | A hidden (no longer present) row owes nothing. | a | pipeline/catch_up.rs::a_hidden_row_owes_nothing |
| ScanCatchUpTests | v36MovesFailureMemory | Migration v36 moves failure memory out of scan_catchup and leaves owed rows. | a | db/migrations.rs::v36_moves_failure_memory_out_of_scan_catchup (read round 16: runs the real shared v36 SQL over failure, owed and unparseable rows and the cascade) |
| ScanCatchUpTests | analysisClearsFailureMemory | A full analysis forgets a remembered failure. | a | dbwriter.rs::three_failures_of_the_same_file_are_remembered_and_a_success_forgets_them |
| ScanCatchUpTests | healLookupUsesIndexes | The rename-heal lookup uses indexes, not a scan. | d | Rust heal tests check results (heal_lookup_*), not the query plan. Missing: EXPLAIN QUERY PLAN assertion in dbwriter.rs; cost: slow scans on 100k-file catalogs |
| ScanCatchUpTests | twinInheritsMarker | A copy of an identical file inherits what the original owes. | a | dbwriter.rs::a_copy_owes_what_its_twin_owes_without_the_twins_own_year_and_camera_tags |
| ScanCatchUpTests | tagCatchUpResumes | Catch-up replaces auto tags, keeps user tags, clears the marker, resumes after cancel. | a | pipeline/catch_up.rs::a_cancelled_pass_keeps_the_rest_owed_and_the_next_one_finishes_it, mac_tags_and_text_after_the_pass_equal_the_inline_scan, stale_changed_failing_and_out_of_scope_files_are_handled_without_touching_their_tags |
| ScanCatchUpTests | changedFileIsLeft | A file changed since the scan is left for the next scan. | a | pipeline/catch_up.rs::stale_changed_failing_and_out_of_scope_files_are_handled_without_touching_their_tags |
| ScanCatchUpTests | carefulReadCatchUp | A careful read the scan could not run is owed, then done by catch-up. | a | pipeline/catch_up.rs::a_careful_read_the_swift_engine_left_owed_is_finished_and_its_tags_are_kept; scan.json scan-cold-careful-reader-owes-the-careful-read |
| ScanCatchUpTests | deferredMatchesInline | Deferred tagging plus catch-up stores what inline tagging stores. | a | pipeline/catch_up.rs::portable_tags_and_text_after_the_pass_equal_the_inline_scan, mac_tags_and_text_after_the_pass_equal_the_inline_scan |
| ScanCatchUpTests | coordinatorProgress | Catch-up progress grows the scan counters and never rolls them back. | d | coordinator.rs tests not matched to catch-up progress. Missing: catch_up.rs test asserting monotone coordinator counters |

### ScanDecodeAheadTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ScanDecodeAheadTests | sameResultsAsWithoutDecodeAhead | Decode-ahead gives every file the result it gets without it. | c | Swift ScanDecodeAhead is dormant/off by default (DECISIONS 2026-11-13); Rust has its own pre-decode (tagging.rs predecode_* tests) |
| ScanDecodeAheadTests | preparedImageSkipsTheWorkerDecode | A prepared image carries its decoded frame and time into the result. | c | Swift-only |
| ScanDecodeAheadTests | cancellationReleasesEverything | A stopped scan returns, releases frames and leaves no twin claim. | c | Swift-only; Rust analog tagging.rs::decoder_blocked_on_full_output_exits_on_cancel |
| ScanDecodeAheadTests | pauseHoldsDecoders | A paused scan decodes nothing until resume. | d | Rust: no test that pause stops decoders (coordinator.rs pause tests not matched to tagging decode). Missing: tagging.rs test with pause flag set before decoders start |
| ScanDecodeAheadTests | memoryAdmission | Frames decode ahead only while memory is free and nothing interactive runs. | a | pipeline/tagging.rs::predecode_budget_weighs_frames_by_actual_bytes, scan_files_wait_for_admission_instead_of_failing |

### DecodeAheadQueueTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| DecodeAheadQueueTests | fillOrder | Items leave in fill order, not reservation order. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | countBackPressure | A full queue holds the next reservation until an item is taken. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | byteBackPressure | The byte budget holds reservations back; a smaller real frame frees room. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | oversizedItemAlone | One item larger than the budget is admitted when nothing is held. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | idleConsumerAddsRoom | An idle consumer makes room for one more decode. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | finishDrains | finish lets consumers drain and waits for a decode in progress. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | abandonReleases | An abandoned reservation ends the queue for waiters. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | cancelDropsEverything | cancel drops held items, wakes waiters, discards late fills. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | taskCancellation | A cancelled task stops waiting promptly and the queue stays usable. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | statistics | Hit, miss and depth statistics report whether consumers waited. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |
| DecodeAheadQueueTests | depthSwitch | FILEID_DECODE_AHEAD picks the depth; unset uses the default. | c | Swift-only queue type; Rust uses a separate bounded pre-decode (tagging.rs predecode_budget_* tests) |

### ScanPipelinePerfTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ScanPipelinePerfTests | skipPredicateUnchanged | Unchanged file (same size and mtime) skips. | a | platforms/windows/src/engine/src/pipeline/discovery.rs::the_skip_decision_follows_size_and_modified_time_over_the_whole_matrix |
| ScanPipelinePerfTests | skipPredicateModified | A changed mtime never skips. | d | see DiscoveryTests.backdatedEditNotSkipped |
| ScanPipelinePerfTests | skipPredicateBackdatedMtime | A backdated same-size edit reprocesses. | d | same |
| ScanPipelinePerfTests | skipPredicateSize | A size change never skips. | d | same |
| ScanPipelinePerfTests | skipPredicateNilMtime | A one-sided nil mtime never skips. | d | same |
| ScanPipelinePerfTests | discoverySkipSet | Incremental walk drops current files; failed/changed/unknown reprocess. | a | scan_session.rs::files_that_keep_failing_join_the_skip_set_with_their_stored_size_and_date (read round 16: given-up, two-try, other-build, owed and settled rows), pipeline/discovery.rs::the_skip_decision_follows_size_and_modified_time_over_the_whole_matrix (changed rows re-read), scan_session.rs::skip_set_excludes_hydrated_placeholder_keeps_scanned_file |
| ScanPipelinePerfTests | streamingMatchesWalk | walkStreaming emits exactly the files walk returns. | c | Swift has two walk APIs; Rust has one streaming walk |
| ScanPipelinePerfTests | decoupledCommitNoLoss | Draining more than two batches through the committer commits every row. | a (round 16) | platforms/windows/src/engine/src/pipeline/dbwriter.rs::the_writer_commits_every_row_across_more_than_two_batches_and_the_tail (round 16: sends two batches and a tail of 7 through run(); failed when the tail flush was removed). The earlier pins, row_count_equals_distinct_paths and a_full_database_fails_the_batch_whole_and_the_writer_recovers, never drive run() across batches |

### TaggingLoadSizeTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TaggingLoadSizeTests | honorsPassedSizeWithoutRestat | A sub-256 passed size skips decode though the disk file is larger. | c | Swift Tagging.load heuristic; no Rust counterpart |
| TaggingLoadSizeTests | decodesWhenPassedSizeAboveFloor | A passed size at or above 256 still decodes (control). | c | Swift-only |
| TaggingLoadSizeTests | zeroSizeFallsBackToStatAndDecodes | A zero discovered size re-stats and still decodes. | c | Swift-only |
| TaggingLoadSizeTests | ignoresEmbeddedThumbnail | Bounded decode ignores undersized embedded JPEG thumbnails. | c | stays, moves to SharedTests: FileIDImaging/BoundedImageDecode.swift is shared (Rust analog tagging.rs::mpf_preview_replaces_the_full_decode_under_the_mac_rules) |
| TaggingLoadSizeTests | jpegDecodeSizes | JPEGs decode to the target size with or without a thumbnail. | c | stays, moves to SharedTests (FileIDImaging) |
| TaggingLoadSizeTests | largePDFBypassesExtraction | PDFs over 20 MiB bypass extraction during scan tagging. | d | tagging_mac.rs pdf_row has a `large` flag but no test. Missing: tagging_mac.rs test with a >20 MiB PDF asserting no extraction (OOM/hang on huge PDFs) |

### HEIFDecoderTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| HEIFDecoderTests | matchesImageIO | Tiled HEICs decode to ImageIO's size, orientation, colours. | c | stays, moves to SharedTests: FileIDImaging/HEIFDecoder.swift is shared |
| HEIFDecoderTests | declinesUnhandledInput | Unhandled layouts return nil for ImageIO. | c | stays, SharedTests |
| HEIFDecoderTests | survivesCorruption | Corrupt or truncated HEICs never crash. | c | stays, SharedTests (Rust fuzz: pipeline/decoder_fuzz_tests.rs covers the Rust decoders only) |
| HEIFDecoderTests | planeBudgetWaits | The plane budget holds a second large decode until the first releases. | c | stays, SharedTests |
| HEIFDecoderTests | scanUsesDecoder | The scan loads HEICs through the HEIF decoder. | c | Swift engine call site disappears; Rust scans use adapters/ScanImages.swift (AppleKitTests/ScanAdapterTests should assert this) |
| HEIFDecoderTests | analysisUsesDecoder | Deep Analyze, face clustering and Takes load HEICs via the HEIF decoder. | c | Swift engine call sites disappear; check adapters Faces*/VLMExports use FileIDImaging loader |

### JPEGPreviewTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| JPEGPreviewTests | usesPreviewUpright | A camera JPEG's large MPF preview stands in for the full image, upright for every orientation. | c | stays, SharedTests (FileIDImaging/JPEGPreview.swift); Rust analog pipeline/mpf_preview.rs::finds_the_large_thumbnail_behind_the_primary, tagging.rs::mpf_preview_replaces_the_full_decode_under_the_mac_rules |
| JPEGPreviewTests | scalesLargePreview | A preview larger than the target is scaled. | c | stays, SharedTests |
| JPEGPreviewTests | declines | Files without a usable preview fall back to the full image. | c | stays, SharedTests; Rust mpf_preview.rs::shape_check_allows_sony_stretch_but_not_crops |
| JPEGPreviewTests | corruptedSegments | Corrupted MPF segments never crash. | c | stays, SharedTests; Rust decoder_fuzz_tests.rs |

### TwinReuseTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TwinReuseTests | (claim owns, waiters take result) | The first claim owns; later claims wait then take the published result. | a | pipeline/twin_reuse.rs::first_claim_owns_and_later_claims_follow_the_outcome |
| TwinReuseTests | (failed owner never reused) | A failed owner is never reused; the next copy analyzes itself. | a | pipeline/twin_reuse.rs::a_waiter_of_a_failed_owner_is_released_when_another_copy_takes_over; tagging.rs::a_waiting_copy_reuses_a_finished_owner_and_decodes_only_when_it_failed |
| TwinReuseTests | (waiters wake on publish) | Waiters wake on publish whether it lands before or after the wait. | a | pipeline/twin_reuse.rs::waiters_wake_on_publish_even_when_it_lands_first |
| TwinReuseTests | (wait timeout frees content) | A wait that times out returns nothing and frees the content for the next copy. | a | tagging.rs::a_waiting_copy_gives_up_before_the_per_file_timeout |
| TwinReuseTests | (cancelled wait returns) | A cancelled wait returns promptly. | d | No Rust twin_reuse cancel test. Missing: twin_reuse.rs cancelled-waiter test (hang on cancel) |
| TwinReuseTests | (result before model load not reused) | A result from before a model loaded is not reused. | d | Missing: twin_reuse.rs model-generation test; cost: stale tags reused across a model change |
| TwinReuseTests | (memory budget, oldest dropped) | Held results stay under a memory budget, oldest dropped first. | d | Missing: budget eviction test in twin_reuse.rs; cost: memory growth |
| TwinReuseTests | (idle drop) | Results are dropped once the registry has been idle. | d | Missing: idle expiry test |
| TwinReuseTests | (reset) | reset forgets results and releases waiters. | d | Missing: reset test; cost: stuck waiters between scans |
| TwinReuseTests | (copy takes twin output, own path/dates/year) | A copy takes the twin's model output but keeps its own path, dates, inode and Year tag. | a | pipeline/dbwriter.rs::a_copy_takes_over_its_twins_rows_and_keeps_its_own_year |
| TwinReuseTests | (copy keeps EXIF time and camera tag) | A copy with an EXIF capture time keeps that time and its camera tag. | a | platforms/windows/src/engine/src/pipeline/tagging_mac.rs::a_twin_row_keeps_its_own_dates_and_leaves_the_analysis_to_the_writer (EXIF capture time and the Year tag taken from it), pipeline/dbwriter.rs::a_copy_takes_the_camera_and_location_its_bytes_share_with_the_twin (camera and location; read round 16) |
| TwinReuseTests | (only full-hash files participate) | Only files stored with a full-file hash take part. | d | Missing: twin_reuse/tagging test that a sampled-hash (over-cap) row is not reused; cost: wrong tags on near-identical large files Round 16: pipeline/dbwriter.rs::a_copy_never_takes_over_a_failed_row_a_non_image_a_sampled_hash_or_a_row_with_reuse_switched_off pins the length-32 guard; an over-cap composite hash is also 32 bytes, so the SQL cannot tell it from a full hash and the decoder claim path is untested. Still open. |
| TwinReuseTests | (in-memory hash equals stored hash) | Hashing bytes in memory gives the stored file hash. | a | util/content_hash.rs::full_hash_is_sha256_for_cross_platform_parity; doc_extract extract_text_bytes_equivalent_to_path (analog) |
| TwinReuseTests | (identical images analyzed once, jpg/heic) | Byte-identical images are analyzed once and each keeps its own row (jpg, heic). | a | tagging.rs::identical_images_all_complete_when_no_analysis_can_be_shared, a_copy_of_an_image_in_flight_is_hashed_but_not_decoded; dbwriter::coexisting_byte_identical_copies_stay_distinct_rows |
| TwinReuseTests | (failed image not offered) | An image that fails is not offered to its copies. | a | tagging.rs::a_waiting_copy_reuses_a_finished_owner_and_decodes_only_when_it_failed |

### CatalogTwinReuseTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogTwinReuseTests | (second copy takes stored analysis) | A second copy takes the stored analysis (tags, faces, text, embedding), own path and Year, no model run. | a | dbwriter.rs::a_copy_of_an_earlier_scan_is_served_from_the_catalog |
| CatalogTwinReuseTests | (date change keeps row, faces, people) | A file whose date changed but bytes did not keeps its row, faces and people. | d | Closest: dbwriter::a_time_a_bit_apart_keeps_the_stored_number_and_a_real_change_updates_it. Missing: touch-only rescan keeps face_prints and person assignments |
| CatalogTwinReuseTests | (owed row not reused) | A row the catch-up still owes work is not reused. | a | platforms/windows/src/engine/src/pipeline/dbwriter.rs::a_copy_owes_what_its_twin_owes_without_the_twins_own_year_and_camera_tags (read round 16: Rust does not skip a twin that owes work, the copy inherits the debt; the outcome, no stale tags kept, is pinned) |
| CatalogTwinReuseTests | (row by other models not reused) | A row made by other models is not reused. | d | Missing: dbwriter test with differing model key; cost: stale tags on model upgrade Round 16: Rust does NOT hold this. The twin lookup (TWIN_LOOKUP_SQL, COMPLETE_IMAGE_SQL) has no model condition, so a copy takes tags a previous model wrote. Not fixed (a design decision, not a few lines). |
| CatalogTwinReuseTests | (failed/other kinds/reuse off never reuse) | Failed rows, other kinds, and reuse switched off never reuse. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/dbwriter.rs::a_copy_never_takes_over_a_failed_row_a_non_image_a_sampled_hash_or_a_row_with_reuse_switched_off (failed, non-image, short hash, no tags or vector, reuse off, and an intact twin as the positive control). Failed when the twin SQL lost its failed/kind/length guard |

### ScanTaggingAdmissionTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ScanTaggingAdmissionTests | (reports time blocked by interactive CPU) | Scan tagging reports time blocked by interactive CPU work. | d | Missing: resource_scheduler.rs test of blocked-time accounting; cost: presentation (progress telemetry) |
| ScanTaggingAdmissionTests | (two files share one CPU unit) | Two scan files share one CPU unit; a third waits. | d (moved from a, round 16) | pipeline/tagging.rs::scan_files_wait_for_admission_instead_of_failing (read round 16: pins that scan files wait for memory instead of failing, not that two files share one CPU unit and a third waits); resource_scheduler.rs::aggregate_budgets_cancellation_and_drop_release is about leases, not scan files. Presentation and timing only. |

### ScanModelResidencyTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ScanModelResidencyTests | freesAfterTheIdlePeriodAndNotBefore | Models free after the idle period and not before. | a | models/idle.rs::parks_after_the_idle_period_and_reloads_on_next_use |
| ScanModelResidencyTests | useRestartsTheIdlePeriod | Use restarts the idle period. | a | platforms/windows/src/engine/src/models/idle.rs::using_a_model_restarts_its_idle_period (round 16; failed when the use time update was removed), a_model_in_use_is_never_parked_and_a_failed_load_retries |
| ScanModelResidencyTests | aRunningScanKeepsTheModelsAndTheIdlePeriodStartsAtItsEnd | A running scan keeps models; idle timing starts at its end. | a | pipeline/scan_models.rs::releasing_idle_models_keeps_adapter_models_a_running_scan_holds |
| ScanModelResidencyTests | nestedHoldsNeedEveryEnd | Nested holds need every end before release. | a | pipeline/scan_models.rs::adapter_models_unload_with_the_last_holder |
| ScanModelResidencyTests | aDescriptionModelLoadFreesIdleModelsOnceAndReloadStartsFresh | Loading a description model frees idle scan models once; reload starts fresh. | a | pipeline/scan_models.rs::a_resident_stack_is_reused_for_the_same_key_and_freed_for_another, releasing_for_another_model_is_counted |
| ScanModelResidencyTests | aSweepWithNothingLoadedDoesNothing | A sweep with nothing loaded does nothing. | d | Missing trivial no-op test in scan_models.rs; cost: presentation/low |

### ModelResidencyTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ModelResidencyTests | sharedMemoryAdmissionReservesSystemHeadroom | Shared-memory admission reserves system headroom. | a | util/model_memory.rs::shared_memory_admission_reserves_system_headroom |
| ModelResidencyTests | modelSwitchWaitsForInferenceAndCancellationDoesNotStrandQueue | A model switch waits for inference; cancel does not strand the queue. | a | models/resident_owner_tests.rs::resident_owner_waiting_checkout_cancel_preserves_current_job (read round 16: a sweep does not retire a busy lease, a cancelled waiting checkout leaves the current job and loads once), resource_scheduler.rs::cancelled_pending_admission_does_not_leak_or_block_following_work |
| ModelResidencyTests | alreadyCancelledAdmissionsLeaveCapacityAvailable | Already-cancelled admissions leave capacity available. | a | resource_scheduler.rs::cancelled_pending_admission_does_not_leak_or_block_following_work |
| ModelResidencyTests | cancellationDuringHandoffDoesNotLeakModelCapacity | Cancel during handoff does not leak capacity. | a | resource_scheduler.rs::aggregate_budgets_cancellation_and_drop_release |

### ResourceSchedulerTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ResourceSchedulerTests | (competing work waits for release) | Competing work waits until the shared memory reservation is released. | a | resource_scheduler.rs::aggregate_budgets_cancellation_and_drop_release |
| ResourceSchedulerTests | (CPU/storage independent of memory) | CPU and storage reservations arbitrate independently of memory. | d | Aggregate test uses all three at once; no isolation assertion. Missing: per-dimension test |
| ResourceSchedulerTests | (resident model keeps memory) | A resident model keeps memory after loading capacity is released. | a | resource_scheduler.rs::aggregate_budgets_cancellation_and_drop_release (retain_memory) |
| ResourceSchedulerTests | (interactive overtakes background) | Interactive work overtakes queued background work without starving it. | a | resource_scheduler.rs::interactive_waiters_precede_background_and_cancel_cleanly |
| ResourceSchedulerTests | (cancel removes only that waiter) | Cancelling a queued reservation removes only that waiter. | a | resource_scheduler.rs::cancelled_pending_admission_does_not_leak_or_block_following_work |
| ResourceSchedulerTests | (scheduled work waits, releases lease) | Scheduled work waits for foreground capacity and releases its lease. | a | resource_scheduler.rs::patient_admission_waits_out_interactive_leases_and_low_memory |
| ResourceSchedulerTests | (lease released on failure) | Scheduled work releases its lease when the operation fails. | d | Missing: lease-on-error test through the scheduled-run wrapper |
| ResourceSchedulerTests | (cancel releases lease) | Cancelling scheduled work releases its lease. | a | resource_scheduler.rs::patient_admission_is_bounded_cancellable_and_refuses_impossible_demands |
| ResourceSchedulerTests | (observes cancel while retrying) | Scheduled work observes cancellation while retrying capacity. | a | resource_scheduler.rs::patient_admission_is_bounded_cancellable_and_refuses_impossible_demands |
| ResourceSchedulerTests | (indexing defers to interactive) | Background indexing defers when resident interactive work owns the budget. | a | resource_scheduler.rs::interactive_waiters_precede_background_and_cancel_cleanly (read round 16: the interactive waiter is admitted first); resident_budget_conflict_is_deferred_instead_of_waiting_on_itself is a different rule (resident memory) |

### JobQueueTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| JobQueueTests | interactiveJobsOvertakePendingBackgroundWorkAndKeepFifo | Interactive jobs overtake pending background work, FIFO within a class. | d | job_queue.rs has no tests and no priority (single FIFO sidebar tracker). Missing: push/promote_next/finish test for queueState order; cost: presentation (sidebar order) - real ordering is enforced in server.rs gates |

### JobQueueChatLaneTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| JobQueueChatLaneTests | queuedChatSummaryDoesNotBlockDescribeCommands | A queued chat summary does not block Describe commands. | d | Missing server.rs test that chat does not take the deep-analyze slot; cost: wrong results (Describe stalls) |
| JobQueueChatLaneTests | aFinishingJobReleasesItsSlotWithinTheGraceWindow | A finishing job releases its slot within the grace window. | a | server.rs::the_deep_analyze_slot_is_free_when_the_terminal_event_can_be_read |
| JobQueueChatLaneTests | aStillRunningJobIsReportedBusyAfterTheTimeout | A still-running job is reported busy after the timeout. | a | server.rs::a_running_deep_analyze_still_refuses_a_second_one |

### IPCSinkCoalescingTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| IPCSinkCoalescingTests | (scanComplete survives progress flood) | A buffered scanComplete survives progress-flood coalescing. | a* | ipc/sink.rs::terminal_phase_send_is_never_dropped_under_backpressure (phase; scanComplete itself not asserted ~); Rust sink does not coalesce, it drops progress via try_send Read round 16: the test pins that send() awaits capacity where try_send drops, for a PhaseChanged; that the scan-complete event is emitted through send() is not tested. |

### IPCSinkEvictionTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| IPCSinkEvictionTests | (terminal completions pinned) | Every terminal completion event is classified critical. | d | Rust sink has no classification; terminal sends use send().await by call site. Missing: test that each terminal EventPayload is emitted via send (not try_send) in server.rs/commands; cost: app hangs waiting for a dropped *Complete |
| IPCSinkEvictionTests | (progress not pinned) | Progress is evictable. | c | Swift classifier; Rust try_send drops progress by construction |
| IPCSinkEvictionTests | (deepAnalyzeComplete + restructureApplyResult reach wire under flood) | Those terminals reach the wire under a progress flood. | d | terminal_phase_send test covers PhaseChanged only. Missing: same for DeepAnalyzeComplete/RestructureApplyResult; cost: stuck Describe/Organize UI after apply (data already moved) |
| IPCSinkEvictionTests | (drainAndClose flushes terminal) | drainAndClose flushes a buffered terminal event then closes. | a (closed round 16; was a ~) | platforms/windows/src/engine/src/ipc/sink.rs::closing_the_channel_writes_and_flushes_every_buffered_event_including_the_terminal_one (round 16: 50 events and a terminal one are buffered, the channel closes, every line is on the stream; failed when both flushes were removed) |

### IPCSinkRestructurePlanPinTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| IPCSinkRestructurePlanPinTests | (restructurePlan classified critical) | restructurePlan is classified critical, also detected in serialized form. | c | Swift classifier; see gap below |
| IPCSinkRestructurePlanPinTests | (restructurePlan reaches wire under flood) | restructurePlan reaches the wire under a full buffer. | d | Missing: Rust test that RestructurePlan is sent with send().await; cost: plan never shown, user cannot apply |

### HardwareTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| HardwareTests | intelNoSMTDoubleCount | Intel 6C/12T worker cap uses physical cores. | a | platform.rs::worker_cap_non_hybrid_8c (read round 16: asserts the Rust formula, physical cores + max(1, cores/2), giving 12 for 8 physical cores and 16 threads, not the Swift value; it does pin that logical threads are not double counted) |
| HardwareTests | intelDegradedClampsToLogical | A degenerate SMT-doubled P count clamps to logical. | d | No Rust clamp test for Mac-hosted cap; Rust worker_cap is a separate formula |
| HardwareTests | (M1 Pro keeps 14) | Apple Silicon M1 Pro keeps the tuned 14. | a | platform.rs::worker_cap_m1_pro_equivalent |
| HardwareTests | (Ultra caps at 32) | Mac Studio Ultra caps at 32. | d | Missing Rust test for the 32 cap |
| HardwareTests | (cap bounded 4..32) | The cap is bounded to [4,32]. | d | Missing bound test; cost: perf/oversubscription |
| HardwareTests | (live cap sane on CI) | The live cap is sane on the host. | c | Swift Hardware (FileIDImaging/Hardware.swift) stays if used by shared code; move to SharedTests |

### ModelBenchmarkTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ModelBenchmarkTests | medianHandlesOddEvenAndNonFiniteRuns | Median handles odd, even and non-finite runs. | a | models/benchmark_tests.rs::median_handles_even_odd_and_empty (read round 16: the NaN case is asserted) |
| ModelBenchmarkTests | jsonValidityAcceptsFencedObjectsOnly | JSON validity accepts fenced objects only. | d (moved from a, round 16) | answers_normalize_to_comparable_tokens does not touch JSON validity. Rust scores the structured pass with pipeline/timeline_analysis.rs::parse_moments; no benchmark test asserts the valid-rate counting. Presentation only. |
| ModelBenchmarkTests | probeMatchesWholeWordsOnly | A probe matches whole words only. | a* | models/benchmark_tests.rs::answers_normalize_to_comparable_tokens, probes_render_deterministically_with_expected_answers (read round 16: Rust compares the first meaningful word of the answer, a different rule from the Swift whole-word match; its own rule is pinned) |
| ModelBenchmarkTests | workloadsCoverVisionOnlyForVisionModels | Vision workloads run only for vision models. | d | Missing: benchmark_server_tests.rs test that text-only models skip image probes |
| ModelBenchmarkTests | syntheticCardIsRedAtTheCentreAndDeterministic | The synthetic card is red at the centre and deterministic. | a | benchmark_tests.rs::probes_render_deterministically_with_expected_answers |
| ModelBenchmarkTests | fixedTileArchitecturesKeepTheirOwnImageSize | Fixed-tile architectures keep their own image size. | c | stays, moves to SharedTests (FileIDVLMCore/ModelBenchmarkRunner.swift is shared) |
| ModelBenchmarkTests | mlxCacheCapScalesWithModelAndHonoursOverride | The MLX cache cap scales with model size and honours override. | c | stays, SharedTests (FileIDVLMCore) |
| ModelBenchmarkTests | fingerprintNamesChipMemoryAndOS | The hardware fingerprint names chip, memory and OS. | d (moved from a, round 16) | No test asserts the shape of mac_fingerprint (chip, GB, cores, macOS major.minor); stored_results_round_trip_in_the_swift_format uses fixed strings and the conformance case uses a {hardware} placeholder. A wrong string hides this Mac's stored results. Low rank. |
| ModelBenchmarkTests | uninstalledModelIsRejectedWithoutDownloading | An uninstalled model is rejected without downloading. | d | Missing: commands/benchmark.rs test; cost: unexpected network download (violates no-egress rule) |
| ModelBenchmarkTests | storeKeepsOnlyThisMachinesResults | The store keeps only this machine's results. | a | benchmark_tests.rs::stored_results_round_trip_in_the_swift_format, models.json models-mac-benchmarks-shared-format |
| ModelBenchmarkTests | idleTimeoutDefaultsToFiveMinutesAndHonoursOverride | Idle timeout defaults to 5 minutes and honours override. | d | models/idle.rs tests use explicit durations. Missing: default and env override test |
| ModelBenchmarkTests | peakMemoryIsDecimalGigabytes | Peak memory is reported in decimal GB. | a (closed round 16; was a ~) | models/benchmark_tests.rs::peak_memory_is_reported_in_decimal_gigabytes (failed when the divisor was 2^30) |

### CarefulTextReadTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CarefulTextReadTests | onlyTheLatinValueTurnsOffLanguageDetection | Only the Latin language value turns off language detection. | c | stays, moves to SharedTests/AppleKitTests (FileIDImaging/CarefulTextRead.swift) |
| CarefulTextReadTests | (gated OCR test) | Gated by FILEID_OCR_TESTS: careful OCR reads known text. | c | stays, AppleKitTests |

### ORTSessionsTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ORTSessionsTests | (per-model folders in engine state) | Each model file, format and runtime gets its own folder in engine state. | c | Swift ORTSessions removed; Rust analog models/runtime_cache_tests.rs (runtime cache paths) |

### RamPlusBackendTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| RamPlusBackendTests | gpuMatchesOnnxRuntime | MLX RAM++ matches ONNX Runtime output (installed models only). | c | stays, AppleKitTests/SharedTests: FileIDImaging/RamPlusMLX.swift; Rust ORT path models/ram_plus.rs has no parity test against MLX |
| RamPlusBackendTests | gpuThroughput | MLX RAM++ throughput (benchmark). | c | perf probe, model-gated; stays optional |
| RamPlusBackendTests | executionProviderSweep | EP sweep (opt-in). | c | perf probe, Swift ORT only; disappears |

### CLIPVisionMLXTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CLIPVisionMLXTests | gpuEncoderMatchesOnnxRuntime | MLX CLIP encoder matches ONNX Runtime (installed models only). | c | stays, AppleKitTests/SharedTests (FileIDImaging/CLIPVisionMLX.swift); Rust/MLX parity on the ONNX side is untested (see Gaps) |
| CLIPVisionMLXTests | gpuThroughput | CLIP MLX throughput. | c | perf probe, stays optional |

### SpeechPermissionTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| SpeechPermissionTests | bareProcessDoesNotAsk | A process without a speech usage description never asks, so macOS cannot kill it. | d | Rust moved the permission call to FileIDAppleKit/MediaSpeech.swift; no test there. Missing: AppleKitTests test of the Info.plist guard; cost: the hosted engine is killed by macOS on speech use (crash) |
| SpeechPermissionTests | (VideoFramePoolTimingTests: frame just before a step) | A frame just before a step counts as that step, so the next one is not a near-duplicate. | d | Swift VideoFramePool; Rust video frame sampling in MediaFrames.swift/apple/media.rs. Missing: AppleKitTests/MediaAdapterTests timing test; cost: duplicate frames in timeline |

### MLXErrorScopeTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| MLXErrorScopeTests | aBroadcastErrorIsThrown | An MLX shape error is thrown, not fatal. | c | stays, moves to SharedTests (FileIDVLMCore catchingMLX); metallib-gated |
| MLXErrorScopeTests | anAnalysisWithAnMLXErrorFailsThatFile | An MLX error in analysis fails that file with an 'Inference failed' result, not the engine. | c | Swift DeepAnalyzeRunner path; Rust analog FileIDAppleVLM/VLMHost.swift withError per request (untested; AppleKitTests/VLMExportsTests should cover) |

### VisualModelEvaluationTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| VisualModelEvaluationTests | (download evaluation model) | Downloads the evaluation model (FILEID_DOWNLOAD_EVALUATION_MODEL). | c | opt-in env-gated evaluation harness, not a regression test; disappears or stays as manual tooling |
| VisualModelEvaluationTests | (evaluation frames) | Runs evaluation frames (FILEID_EVALUATION_FRAMES). | c | opt-in env-gated evaluation harness, not a regression test; disappears or stays as manual tooling |
| VisualModelEvaluationTests | (evaluation goal) | Runs the evaluation goal (FILEID_EVALUATION_GOAL). | c | opt-in env-gated evaluation harness, not a regression test; disappears or stays as manual tooling |
| VisualModelEvaluationTests | (tokenizer evaluation) | Tokenizer evaluation (FILEID_EVALUATION_TOKENIZER). | c | opt-in env-gated evaluation harness, not a regression test; disappears or stays as manual tooling |

### Totals

a: 91, a*: 4, b: 1, c: 52, d: 48, total 196 (round 16: 7 gaps closed, 4 rows moved from a to d, 4 narrowed to a*)

### Gaps (d), ranked

1. (Closed round 16, except that Rust and Swift differ on a stored nil time; see the Discovery rows.) ScanPipelinePerfTests / DiscoveryTests skip predicate (size, mtime, nil, drift): data loss/wrong results: an edited file whose mtime is backdated is silently skipped, stale tags and text stay. Suggest: discovery.rs unit test of the skip decision over a (size,mtime) matrix; first decide Rust both-nil behaviour.
2. DiscoveryTests.skipsLargeFiles / discoversTextData: wrong results: oversize files either never indexed or OOM; text data indexed as Other. Suggest: discovery.rs: tempdir walk with oversize tif/pdf and csv/json; kind_from_extension table.
3. IPCSinkEviction/RestructurePlanPin terminal events: wrong results: dropped DeepAnalyzeComplete/RestructureApplyResult/RestructurePlan leaves the app waiting after files were moved or plan never shown. Suggest: ipc/sink.rs or server.rs: flood with try_send fillers, assert each terminal payload is delivered via send().await.
4. (Failed, non-image and short-hash twins closed round 16; the Rust registry holds one bool per hash, so the budget, idle and reset rows have no Rust equivalent; the model-generation rule is not held, see CatalogTwinReuse.) TwinReuse: model-generation, full-hash-only, cancel, budget, idle, reset: wrong results: stale or mis-shared tags/faces across copies; hang on cancel. Suggest: pipeline/twin_reuse.rs unit tests with the Registry directly.
5. (failed/kind/off closed round 16; other-model row is an invariant Rust does not hold.) CatalogTwinReuse: other-model row, failed/kind/off, date-only change keeps faces: wrong results: faces and people dropped on touch, stale analysis after model upgrade. Suggest: pipeline/dbwriter.rs reuse tests with seeded rows.
6. ModelBenchmark.uninstalledModelIsRejectedWithoutDownloading: network egress without consent. Suggest: commands/benchmark.rs: run on an empty models dir, assert error and no downloader call.
7. SpeechPermission.bareProcessDoesNotAsk: crash of the hosted engine on speech use. Suggest: AppleKitTests: MediaSpeech canAsk against two synthetic bundles.
8. TaggingLoadSize.largePDFBypassesExtraction: hang/OOM on huge PDFs in a scan. Suggest: pipeline/tagging_mac.rs: pdf_row with large=true yields no extraction.
9. TextReading careful-read trigger, handwriting label, disagreement, accurate-tier breaker: wrong results: handwriting not labelled or OCR retried forever. Suggest: pipeline/text_reading.rs table tests.
10. DocumentAnalysis corruptPDF retry, classifications, prompts, textOnlyGrounding, metadata-less audio: wrong results: corrupt PDF erases prior metadata; invented visual claims in names. Suggest: deep_analyze_naming.rs and dbwriter.rs rescan tests.
11. ScanCatchUp.healLookupUsesIndexes, coordinatorProgress: slow scans; progress rolling back. Suggest: dbwriter.rs EXPLAIN QUERY PLAN; catch_up.rs counters.
12. ScanDecodeAhead.pauseHoldsDecoders: wrong behaviour: pause does not stop decode. Suggest: tagging.rs pause-flag test.
13. Hardware clamp/cap bounds, ResourceScheduler per-dimension and lease-on-failure, ModelBenchmark vision-only workloads and idle default, JobQueue ordering, ScanTaggingAdmission blocked time, ScanModelResidency empty sweep, SpeechPermission frame-step timing: presentation/perf only. Suggest: platform.rs, resource_scheduler.rs, models/idle.rs, job_queue.rs, benchmark_server_tests.rs, MediaAdapterTests.


### Round 16: second pass over the "~" rows of area B

Every row of this area carrying a "~", "not individually verified" or "not opened" (29 rows) was re-read against the Rust test it names. Result: 12 confirmed and the mark removed; 4 narrowed to a* (the sub-invariant is named in the row); 8 kept as a with a corrected reference or a test added this round; 1 kept as b with the unasserted part named (scanTagging); 4 moved from a to d (DocumentAnalysisTests.targetResolution, ScanTaggingAdmissionTests two-files-share-a-unit, ModelBenchmarkTests.jsonValidityAcceptsFencedObjectsOnly and fingerprintNamesChipMemoryAndOS). The other 167 rows of area B were not re-read.

Closed in round 16 (each test shown to pass and to fail on a broken guard; all in files that are not digest-locked): the discovery skip decision over size, time, nil and drift (`pipeline/discovery.rs`), the text-data and RAW extension kinds, the writer committing every row across batches and the tail (`pipeline/dbwriter.rs`), a copy never taking over a failed, non-image, short-hash or empty twin (`pipeline/dbwriter.rs`), the sink flushing every buffered event on close (`ipc/sink.rs`), benchmark peak memory in decimal GB, the composite hash reading each edge (`util/content_hash.rs`), a model use restarting its idle period (`models/idle.rs`).

Rust does not hold these Swift invariants (recorded, not fixed): a copy takes the tags of a twin written by another model; a stored nil modified time is re-read where Swift skips it; no file size cap on the walk.

Shared Swift code whose only tests are in EngineTests: TaggingLoadSizeTests (FileIDImaging bounded decode) and JPEGPreviewTests (FileIDImaging MPF preview) import `FileIDEngine` and `FileIDImaging`. Neither AppleKitTests nor SharedTests has a test of either file, and SharedTests does not depend on FileIDImaging in Package.swift. When the engine tests are deleted these two suites need to move, with a Package.swift test-target dependency; not done here (Package.swift is outside this task).

## Area C: faces, takes, search, naming, Describe Files


Rows: 292 tests in 41 suites (counts verified with grep -cE '@Test|func test'; parameterised @Test counts once; DeepAnalyzeTests.swift has no struct of that name, it holds 6 suites and 34 tests and is mapped as one group). Categories: a = pinned by Rust unit test, b = shared conformance case, c = Swift-only or shared code that stays, d = gap. Conformance-pinned rows are listed under a with the case name (cat 'a' with 'conformance ...'); no row relies on conformance alone except where the Rust pin column says so. Rust pins were read by test name plus the assertions of the face extract tests and the clustering code paths; rows marked 'partial' or 'not individually verified' were not opened line by line.

### FaceAlignTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceAlignTests | template to template fits identity | Fitting the template onto itself yields a=1, b=0, no translation. | a | models/face_align.rs::identity_fit |
| FaceAlignTests | recovers a known scale + translation | A pure scale and shift is recovered by the similarity fit. | a | models/face_align.rs::translation_recovered (scale part also in shared_alignment_fixtures) |
| FaceAlignTests | recovers a 90 degree rotation | A 90 degree rotation fits as a=0, /b/=1. | a | models/face_align.rs::shared_alignment_fixtures (read round 16: the shared fixture case `rotation` expects [0, -1, 0, 0] and the loop asserts every case within 0.001) |

### FaceAnalysisCacheTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceAnalysisCacheTests | sourceIdentityRejectsAChangedInternalFixture | A face vector is only valid for the source file revision it was computed from. | a | commands/face_extract_mac_tests.rs::pending_lists_faces_without_a_current_vector (changed photo needs faces again) + persist_refuses_stale_rows_and_bad_vectors (moved bbox, resized file refused) |
| FaceAnalysisCacheTests | refreshPreservesIdentityAndSeparatesModelEvidence | Saving a refreshed vector keeps the face's person and name and keeps each model's embeddings separate. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_extract_mac_tests.rs::a_refreshed_vector_keeps_the_face_s_person_and_each_model_keeps_its_own_evidence (failed when `persist` cleared `person_id`) |
| FaceAnalysisCacheTests | staleCompletionAndMalformedVectorCannotOverwriteCorrections | A stale or malformed vector from a finished refresh never replaces the saved face data. | a | commands/face_extract_mac_tests.rs::persist_refuses_stale_rows_and_bad_vectors |
| FaceAnalysisCacheTests | exclusionDoesNotReviveChangedSourceAndManualObservationSurvivesDeletion | Excluding or re-including a face does not un-stale changed-source evidence, and a hand-edited observation survives refresh/deletion. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_extract_mac_tests.rs::excluding_a_face_never_revives_changed_evidence_and_a_hand_edited_observation_survives (failed with the `user_edited=0` guard removed from the v22 observation trigger) |
| FaceAnalysisCacheTests | durableFailureBackoffDoesNotStarveNewFaces | A face in failure back-off is skipped so newer faces still get processed within the page limit, and the back-off doubles. | a | commands/face_extract_mac_tests.rs::failures_back_off_and_double (window and doubling); the limit-1 'does not starve' ordering with an afterFaceID cursor is not asserted |
| FaceAnalysisCacheTests | failureSnapshotCannotSuppressEditedOrHeldFaces | A recorded failure never hides a face whose source was edited or that was excluded. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_extract_mac_tests.rs::a_recorded_failure_never_hides_a_face_whose_box_was_edited_or_that_was_excluded (failed with the box match removed from the failure filter in `pending`) |
| FaceAnalysisCacheTests | failureStateSurvivesReopeningTheDatabase | Failure back-off and a confirmed person's name survive closing and reopening the catalog. | a | commands/face_extract_mac_tests.rs::failures_back_off_and_double (state lives in face_refresh_failures SQL table; no explicit reopen assertion) |
| FaceAnalysisCacheTests | malformedCurrentNamespaceVectorIsEligibleForRepair | A face whose current-model vector is malformed is treated as pending and repaired in place, keeping its person. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_extract_mac_tests.rs::a_malformed_current_vector_is_repaired_in_place_and_keeps_the_person (failed with the norm check removed from `pending`) |

### FaceClusteringDeterminismTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceClusteringDeterminismTests | HNSWIndex: two builds from identical input yield identical kNN orderings | Two index builds from the same vectors answer identically. | a | util/hnsw_index.rs::build_is_deterministic_across_runs |
| FaceClusteringDeterminismTests | IdentityClustering: cluster-ID assignment is stable (sorted-root iteration) | Cluster ids are 1-based and do not depend on input order. | a | pipeline/face_clustering.rs::cluster_ids_are_one_based_and_stable + clustering_partition_is_stable_across_input_order |
| FaceClusteringDeterminismTests | IdentityClustering: running-sum Pass-2 assigns outliers into one core | Pass-2 outliers are folded into exactly one core using a running centroid sum. | a | pipeline/identity_clustering.rs::pass2_rejects_outlier_from_conflicting_core (outlier goes to one core); running-sum equivalence itself not asserted |
| FaceClusteringDeterminismTests | IdentityClustering: shouldCancel aborts the pass at a safe boundary | A cancel request stops clustering at a boundary without a partial result. | d | Rust identity_clustering/face_clustering_mac have no cancel hook or test. Add to pipeline/identity_clustering.rs if cancel is wanted; today clustering cannot be cancelled. |
| FaceClusteringDeterminismTests | clusterShouldCancel: stale scan-cancel ignored; fresh cancel + shutdown honored | A cancel left over from an old scan does not abort a later clustering pass. | d | No Rust test for stale-vs-fresh cancel of a clustering run. Add to commands/face_clustering.rs (single-flight tests live there). |

### FaceClusteringMergeTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceClusteringMergeTests | legacy person centroids cannot cross model spaces | Person centroids from another face model/dimension are never used to inherit ids. | a | commands/face_clustering.rs::centroid_inheritance_rejects_wrong_dimensions_and_invalid_radius + malformed_prior_centroids_fail_closed |
| FaceClusteringMergeTests | mixed, legacy, stale and malformed face caches preserve identities | If face vectors are mixed/legacy/stale, clustering refuses and every person and assignment is left unchanged. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::mixed_legacy_stale_and_malformed_face_caches_are_deferred_and_keep_every_identity (auto-merge and a full pass; failed with the model check removed from `is_current`; the other four faults are each refused by two checks) |
| FaceClusteringMergeTests | raw bridge cannot fuse two protected identities before persistence | A bridging face cannot merge two named people. | a | pipeline/face_clustering.rs::protected_named_bridge_is_split_and_stays_transitively_safe (Round 17 second pass: failed with the named-owner bridge check in consolidation removed.) |
| FaceClusteringMergeTests | raw bridge cannot collapse an explicit different-people pair | A bridge cannot join a pair the user marked different. | a | pipeline/face_clustering.rs::protected_explicit_bridge_is_split_before_consolidation (Round 17 second pass: failed with the split of a different-people pair out of its raw cluster removed.) |
| FaceClusteringMergeTests | protected micro-clusters bypass junk suppression | A named person's tiny low-quality cluster is not dropped as junk. | a | pipeline/face_clustering.rs::suppression_keeps_copied_content_when_protected + embedding_outlier_suppression_skips_protected_cluster |
| FaceClusteringMergeTests | protected identity without current faces is preserved | A person whose faces are not in this pass keeps their row. | a | commands/face_clustering.rs::protected_owner_outside_pool_or_without_faces_is_preserved |
| FaceClusteringMergeTests | stale persistence members fail before destructive SQL | If faces changed after planning, the write is refused before anything is deleted. | a | commands/face_clustering.rs::persistence_plan_rejects_missing_anchor_members + destructive_persistence_sql_rolls_back_atomically (Round 17 second pass: failed with the anchor-membership check removed.) |
| FaceClusteringMergeTests | preserved persons consume the hard person cap | Kept people count against the 8000 person limit. | a (closed round 17; was a*) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::a_plan_that_would_pass_the_person_limit_writes_nothing (the `apply()` check; failed with it removed) and, for `cap()`, platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::people_already_kept_use_up_the_person_cap_and_an_overfull_protected_set_refuses |
| FaceClusteringMergeTests | phase-zero unknown faces do not consume the eligible window | Faces of Unknown people do not use up the per-run face limit. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::faces_of_people_marked_unknown_do_not_use_up_the_face_window (failed with the skip check in `load_partition` removed) |
| FaceClusteringMergeTests | no-work clustering result can report preserved persons | A pass with nothing to cluster still reports the people that were kept. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::a_pass_with_nothing_to_cluster_still_reports_the_people_it_kept (failed when the no-work path reported 0 people) |
| FaceClusteringMergeTests | an is_unknown person is never auto-merged | Auto-merge never joins a person marked Unknown. | a (closed round 14; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::auto_merge_never_joins_a_person_marked_unknown (outcome; four independent guards, none alone is shown to fail it). Was: auto_merge (face_clustering_mac.rs:1252) has no test except the VP join; conformance covers merge SUGGESTIONS only. Add auto_merge tests to face_clustering_mac_tests.rs. |
| FaceClusteringMergeTests | a 'different' verdict pair is never auto-merged | Auto-merge never joins two people the user said are different. | a (closed round 14; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::auto_merge_never_joins_two_people_the_user_said_are_different. Was: auto_merge untested for verdicts (pipeline consolidate_respects_blocked_pair covers the clustering stage, not auto_merge). Add to face_clustering_mac_tests.rs. |
| FaceClusteringMergeTests | search overload leaves every person unchanged | If the merge search exceeds its budget, no person is merged. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::a_merge_search_over_its_edge_budget_merges_no_one (1,415 identical people, 1,000,405 pairs; 1,414 merge as a control; failed with the edge budget removed from the tree search) |
| FaceClusteringMergeTests | oversized evidence is deferred while compatible people can merge | Too-large evidence for one person is skipped, others still merge. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::oversized_evidence_is_deferred_while_other_people_still_merge (outcome only: the 16 KB vector is refused by the column-length CASE, the blob-length check and the norm check; no single one was broken) |
| FaceClusteringMergeTests | a bridge singleton cannot transitively merge two named persons | A single face between two named people cannot chain-merge them. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::a_bridge_face_cannot_chain_two_named_people_together (failed only with both guards removed: the named flag carried to the merged root and the commit filter that moves only unnamed people; either alone still passes) |
| FaceClusteringMergeTests | a title/middle/suffix-only-named person is treated as named | A person with only a title, middle name or suffix counts as named and is protected. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::people_named_only_by_a_title_middle_name_or_suffix_are_never_auto_merged (failed when `has_name` ignored title, middle name and suffix) |
| FaceClusteringMergeTests | persist re-reads identity under the writer lock | Names/verdicts are re-read inside the write transaction so a concurrent rename is not lost. | d | The in-transaction re-read (face_clustering_mac.rs:1417) has no test for a rename between plan and commit. Add to face_clustering_mac_tests.rs. Round 17: still open. `persist` and `auto_merge` call `priors()` on the transaction they commit, and `auto_merge` refuses when priors or verdicts differ from its plan, but there is no seam to rename a person between plan and commit, so no test can show the re-read matters. |
| FaceClusteringMergeTests | a dangling representative_face_id is reconciled | A person pointing at a missing face gets a valid representative. | a | db/person_counts.rs::reconcile_recounts_only_the_named_persons_and_repairs_the_representative |
| FaceClusteringMergeTests | Disk changes after partition selection reject a merge commit | If a photo changed after planning, the merge is not committed. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::a_photo_changed_after_the_partition_was_read_refuses_the_commit (failed with the comparison in `require_unchanged` removed) |
| FaceClusteringMergeTests | person, embedding-row, and embedding-byte preflight caps are independent | Each of the three size limits trips on its own. | d | MAX_PERSONS/MAX_FACES_PER_RUN/byte caps untested. |
| FaceClusteringMergeTests | VP-tree edges equal the deterministic direct sweep | The vantage-point join finds exactly the pairs the brute-force sweep finds. | a | commands/face_clustering_mac_tests.rs::the_vantage_point_join_finds_every_pair_the_sweep_finds |
| FaceClusteringMergeTests | high-dimensional ULP boundaries and singleton rules match the direct predicate | Pairs at the exact cosine threshold are decided identically by both join paths. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::pairs_at_the_exact_thresholds_are_joined_the_same_by_the_tree_and_the_direct_predicate (720 vectors of 512 dimensions, so the tree path runs; failed with `>=` made `>` in `qualifies`; a zero roundoff margin in the tree radius was not caught) |
| FaceClusteringMergeTests | dense and adversarial searches fail before returning a partial graph | An over-dense input errors out instead of returning half a join. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::an_exhausted_join_budget_returns_no_partial_graph (work and edge budgets on both the direct and the tree path; failed with the tree edge budget removed) |
| FaceClusteringMergeTests | low-quality 1-2 face clusters are suppressed; size>=3 and good faces survive | Small low-quality clusters are dropped, size 3+ or good faces stay. | a | pipeline/face_clustering.rs::suppression_drops_low_quality_micro_clusters |
| FaceClusteringMergeTests | a doubleton keeps the cluster when its BEST face clears the floor | A pair survives if one face is good. | a | pipeline/face_clustering.rs::suppression_keeps_pair_with_one_good_face |
| FaceClusteringMergeTests | qualityFloor <= 0 disables suppression (keeps every cluster) | A zero floor keeps all clusters. | a | pipeline/face_clustering.rs::suppression_floor_zero_is_noop |
| FaceClusteringMergeTests | env overrides parse + clamp | Environment overrides are parsed and clamped to safe ranges. | a | pipeline/face_clustering.rs::automerge_threshold_clamps_and_defaults + suppression_env_defaults |
| FaceClusteringMergeTests | held faces attach only to an established, unambiguous person | New faces are only ever suggested, and only to a clear single match. | a | commands/face_clustering_mac_tests.rs::held_faces_are_only_ever_uncertain + commands/face_review_tests.rs::decide_classifies_by_score_and_margin |
| FaceClusteringMergeTests | two faces in one photo never share a person, even through a bridge | Faces from one physical photo are never put in the same person. | a | pipeline/face_clustering.rs::clustering_never_co_locates_faces_from_one_physical_file (Round 17 second pass: failed with the same-file cannot-link removed.) |

### FaceClusterPersistenceTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceClusterPersistenceTests | repeatedClusteringRetainsIdentitiesAndReferences | A second clustering pass keeps person ids and rewrites no face. | a | commands/face_clustering.rs::a_second_pass_keeps_person_ids_and_rewrites_no_face (Round 17 second pass: failed with the changed-owner filter on face writes removed.) |
| FaceClusterPersistenceTests | newClusterDoesNotClearOtherPeople | Adding a new cluster does not delete or reset existing people. | a | commands/face_clustering.rs::people_no_cluster_keeps_are_removed_and_their_faces_released + persisting_writes_only_faces_whose_person_changes (Round 17 second pass: failed with the delete of people no cluster keeps removed.) |
| FaceClusterPersistenceTests | failedPublicationRollsBackAssignmentsAndAnalysis | A failed write leaves assignments and analysis as they were. | a (re-pinned round 17) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::a_persist_that_fails_part_way_leaves_every_person_and_assignment_as_it_was (a write refused at its last statement; failed when `run` committed after a failed persist). The old pin, commands/face_clustering.rs::destructive_persistence_sql_rolls_back_atomically, runs inline SQL only and calls no engine code |
| FaceClusterPersistenceTests | personLimitPreservesExistingPeople | Hitting the person cap leaves existing people untouched. | a (closed round 17; was a*) | platforms/windows/src/engine/src/commands/face_clustering_mac_tests.rs::a_plan_that_would_pass_the_person_limit_writes_nothing (the `apply()` check; failed with it removed) and, for `cap()`, the round 16 tests |

### FaceLandmarkMatchTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceLandmarkMatchTests | sharedAlignmentFixtures | Alignment fits match the shared fixture. | a | models/face_align.rs::shared_alignment_fixtures |
| FaceLandmarkMatchTests | sharedAlignedCropsMatchBitForBit | 112x112 aligned crops are bit-identical to the shared fixture. | a | models/face_align.rs::shared_aligned_crops_match_bit_for_bit |
| FaceLandmarkMatchTests | sharedPaddedCropsMatchTheRustRects | Padded crop rectangles match the shared fixture. | a | models/face_landmark_match.rs::shared_padded_crops_match_swift |
| FaceLandmarkMatchTests | nearbyFaceCannotSupplyLandmarks | A neighbouring face's landmarks are not used. | a | models/face_landmark_match.rs::nearby_face_cannot_supply_landmarks |
| FaceLandmarkMatchTests | displacedOverlappingFaceIsRejected | An overlapping but displaced face is rejected. | a | models/face_landmark_match.rs::displaced_overlapping_face_is_rejected |
| FaceLandmarkMatchTests | ambiguousAndInvalidGeometryAbstains | Ambiguous or invalid geometry gives no landmarks. | a | models/face_landmark_match.rs::ambiguous_and_invalid_geometry_abstains |
| FaceLandmarkMatchTests | batchedAlignmentPreservesPixelsAndRejectsInvalidPoints | Batched alignment equals single alignment and rejects bad points. | a | models/face_align.rs::rejects_invalid_geometry_and_empty_images + rgba_alignment_keeps_alpha_and_needs_a_full_buffer |

### FaceMeshTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceMeshTests | sharedGeometryMatchesTheRustEngine | Eye geometry matches the shared fixture. | a | models/face_mesh.rs::shared_geometry_fixture_matches |
| FaceMeshTests | ratioMapsToOpennessAtTheSharedThreshold | Eyelid gap ratio maps to openness at the shared threshold. | a | models/face_mesh.rs::eyelid_gap_over_width_scores_openness |
| FaceMeshTests | visionFacesBecomeTopLeftPixelDetections | Vision normalised face boxes become top-left pixel boxes. | c | Swift Vision adapter conversion; Rust takes boxes through face_bbox.rs (raw_and_upright_formats...). Stays only if kept in AppleKit adapters (ScanVision.swift). |
| FaceMeshTests | detailImageIsMadeOnceAndOnlyWhenRead | The detail image is decoded lazily and once. | a | commands/catalog_take_auto.rs::a_deferred_detail_decodes_once_and_only_when_read |
| FaceMeshTests | meshDetailIsTheTileSizeDecodeOfLargePhotos | Large photos are decoded at tile size for the mesh. | c | Swift decode path (TakeMediaAnalysis); Rust equivalent covered loosely by catalog_take_auto::face_regions_match_the_photo_resized_to_the_tile_size |
| FaceMeshTests | cacheIDNamesTheEyeModel | The take-signal cache key names the eye model. | c | Swift-only cache id string; Rust cache key in take_scoring.rs signals_round_trip_through_the_cache_json |
| FaceMeshTests | (env-gated) FILEID_FACE_MESH_JOBS | Manual real-model mesh job. | c | Manual, disabled unless env var set. |

### FaceOrientationRefreshTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceOrientationRefreshTests | importedRawBoxesFollowTheDecodedJpegRegion | Raw-orientation face boxes are mapped to the decoded upright region. | a | face_bbox.rs::raw_and_upright_formats_describe_the_same_rotated_subject + commands/face_refresh_tests.rs::pixel_boxes_follow_both_engines_box_formats |

### FaceQueuesTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceQueuesTests | gatedNewFacesAreHeldOutAndOnlyEverUncertain | New faces after people exist are held out and only suggested. | a | commands/face_clustering_mac_tests.rs::held_faces_are_only_ever_uncertain |
| FaceQueuesTests | gatedFacesInAFreshSpaceStayInThePass | In a new face space with no people, faces go through normal clustering. | a | commands/face_review_tests.rs::holdout_takes_new_faces_only_once_people_exist |
| FaceQueuesTests | acceptAllAcceptsSuggestedSkipsPhotosWithThePersonAndUndoes | Accept all assigns suggestions, skips photos already with the person, and undoes. | a | conformance face-review-accept-all-and-undo + commands/face_review_tests.rs::accept_all_accepts_suggested_faces_in_one_operation_and_skips_photos_with_the_person |
| FaceQueuesTests | mergesPairCloseNamedFirstAndSkipUnknownSharedAndFar | Merge suggestions pair close people, named first, skip Unknown, shared-photo, far. | a | commands/face_queues_tests.rs::merges_pair_close_people_named_first_and_skip_unknown_shared_and_far_people |
| FaceQueuesTests | mergeJournalsAndUndoBringsThePersonBack | A merge is journaled and undo restores the person. | a | commands/face_queues_tests.rs::merge_moves_faces_journals_and_undo_brings_the_person_back (Round 17 second pass: failed with the face move back in merge undo (the undo-once check alone does not fail it: a second undo is also refused before it reaches that check) removed.) |
| FaceQueuesTests | mergeIsRefusedWhenThePeopleShareAPhoto | Merging people who share a photo is refused. | a | commands/face_queues_tests.rs::merge_is_refused_when_the_people_now_share_a_photo (Round 17 second pass: failed with the shared-photo refusal in `merge_in` removed.) |
| FaceQueuesTests | notSamePersonKeepsThePairOutUntilUndone | 'Not the same person' hides the pair until undone. | a | commands/face_queues_tests.rs::not_same_person_records_a_verdict_that_keeps_the_pair_out_until_undone (Round 17 second pass: failed with the different-people filter in `blocked_pairs` removed.) |
| FaceQueuesTests | dismissalFollowsTheFacesAndUndoForgetsIt | A dismissal sticks across person-id changes; undo forgets it. | a | commands/face_queues_tests.rs::dismissal_survives_new_person_ids_and_undo_forgets_it |
| FaceQueuesTests | splitFlagsSeparatedGroupsAndUndoRestores | Split suggestions flag separated groups; undo restores. | a | commands/face_queues_tests.rs::split_flags_two_separated_groups_and_keeps_the_representative_face + split_moves_the_group_to_a_new_person_and_undo_restores_it |
| FaceQueuesTests | splitUndoIsRefusedOnceTheNewPersonIsNamed | Split undo is refused after naming the new person. | a | commands/face_queues_tests.rs::split_undo_is_refused_after_the_new_person_was_named (Round 17 second pass: failed with the named-person refusal of split undo removed.) |
| FaceQueuesTests | splitRefusedWhenFacesMovedAndDismissHidesIt | A split is refused if faces moved; dismissing hides it. | a | commands/face_queues_tests.rs::split_is_refused_when_its_faces_moved_and_dismissal_hides_it |
| FaceQueuesTests | twoGroupsSeparatesClearGroups | The two-group splitter separates clear groups deterministically. | a | commands/face_queues_tests.rs::two_groups_is_deterministic_and_separates_clear_groups |
| FaceQueuesTests | personFileCountIgnoresExcludedFaces | A person's file count ignores excluded faces. | a | db/person_counts.rs::recount_stale_corrects_wrong_excluded_and_empty_counts |
| FaceQueuesTests | drainingRecordsEveryStateAndTheNextSeedRetries | Refresh draining records done/missing/offline/failed; next seed retries. | a | commands/face_refresh_tests.rs::draining_records_done_missing_offline_and_failed_and_retries_on_the_next_seed |
| FaceQueuesTests | aPausedJobStopsAndAnotherModelFails | A paused refresh stops between batches; another model fails the job. | a | commands/face_refresh_tests.rs::a_paused_job_stops_between_batches_and_keeps_the_rest_pending + a_different_model_fails_the_job_instead_of_mixing_spaces |
| FaceQueuesTests | anInterruptedJobComesBackPaused | An interrupted refresh job returns paused. | a | commands/face_refresh_tests.rs::an_interrupted_job_comes_back_paused + conformance face-refresh-backlog-recovers-and-reports |
| FaceQueuesTests | enqueueNeedsAModel | Enqueue without a face model is an error. | a | commands/face_refresh_tests.rs::enqueue_needs_a_face_model |
| FaceQueuesTests | classifyTellsMissingFromOffline | A missing photo is told apart from an unreachable location. | a | commands/face_refresh_tests.rs::classify_tells_missing_photos_from_unreachable_locations |

### FaceReviewTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| FaceReviewTests | migrationBackfillsExistingFacesAsBaseline | Existing faces are marked baseline so they never reappear as new. | a (closed round 17; was d) | platforms/windows/src/engine/src/db/migrations.rs::tests::faces_already_in_a_catalog_are_marked_baseline_so_they_never_come_back_as_new (migrates up to v31, adds faces, applies v32; failed with the backfill INSERT removed from v32.sql) |
| FaceReviewTests | holdoutTakesNewFacesOnlyOnceSomeoneIsAssigned | New faces are held out only once someone is assigned. | a | commands/face_review_tests.rs::holdout_takes_new_faces_only_once_people_exist |
| FaceReviewTests | assessmentClassifiesFaces | Faces are classed suggested/uncertain/ambiguous by score and margin. | a | commands/face_review_tests.rs::decide_classifies_by_score_and_margin + new_faces_become_suggestions_without_changing_assignments |
| FaceReviewTests | unknownBestStaysUnmatchedAndBarsCount | If the best match is Unknown the face stays unmatched. | a | commands/face_review_tests.rs::unknown_people_same_photo_and_rejections_never_receive (Round 17 second pass: failed with the same-photo bar in `bars` (the Unknown and rejection guards are separate; the rejection bar failed the last assertion on its own) removed.) |
| FaceReviewTests | constraintsAndVerdictsBarPeople | Rejections and different-people verdicts bar a person from a face. | a | commands/face_review_tests.rs::different_people_verdicts_bar_the_other_person (Round 17 second pass: failed with the verdict bar in `bars` removed.) |
| FaceReviewTests | bestFirstClaimsPersonPerPhoto | The best-scoring face claims a person for its photo. | a | commands/face_review_tests.rs::best_score_claims_the_person_for_its_photo |
| FaceReviewTests | autoAssignIsOptIn | Auto-assign only happens when enabled and confident. | a | commands/face_review_tests.rs::opt_in_auto_assign_assigns_confident_matches_only + conformance face-review-clustering-opt-in-auto-assign |
| FaceReviewTests | finishMarksPoolAndCountsQueue | After a pass the pool is marked done and the queue counted. | a | conformance face-review-clustering-queues-new-faces |
| FaceReviewTests | listPagesFiltersAndRefuses | Review list pages best-first, filters by person, refuses bad queues. | a | commands/face_review_tests.rs::list_pages_best_first_and_filters_by_person + list_marks_low_quality_faces_and_rejects_unknown_queues |
| FaceReviewTests | acceptAssignsAndUndoRestoresSuggestion | Accept assigns and undo restores the suggestion. | a | commands/face_review_tests.rs::accept_assigns_logs_and_undoes |
| FaceReviewTests | acceptRefusesWhenPersonAlreadyInPhoto | Accept is refused if the person is already in the photo. | a | commands/face_review_tests.rs::accept_is_refused_when_the_person_is_in_the_photo (Round 17 second pass: failed with the person-already-in-the-photo refusal removed.) |
| FaceReviewTests | rejectStoresConstraintAndUndoRemovesIt | Reject stores a constraint; undo removes it. | a | commands/face_review_tests.rs::reject_records_a_constraint_that_undo_forgets |
| FaceReviewTests | ignoreFilesUnderNewUnknownAndUndoDeletesIt | Ignore files the face under a new Unknown; undo deletes it. | a | commands/face_review_tests.rs::ignore_files_the_face_under_a_new_unknown_person |
| FaceReviewTests | reassignRecordsCorrectionAndConstraint | Manual reassign records a correction and constraint. | a | commands/face_review_tests.rs::manual_reassignment_records_a_constraint_and_settles_the_face |
| FaceReviewTests | invalidRequestIDsAreRejected | Malformed request ids are refused. | a | commands/face_review_tests.rs::request_ids_follow_the_nonce_pattern |

### ReassignFaceTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ReassignFaceTests | movesFaceAndReconcilesBothPeople | Moving a face updates the file counts and representative of both people. | a | conformance people-reassign-records-a-correction + db/person_counts.rs::reconcile_recounts_only_the_named_persons_and_repairs_the_representative; the end-to-end handler (bulk.rs handle_reassign_face) has no unit test |
| ReassignFaceTests | newPersonIsCreatedAndEmptiedUnnamedSourceIsRemoved | Reassign to a new person creates it; an emptied unnamed source is deleted. | a (closed round 14; was d) | platforms/windows/src/engine/src/commands/bulk.rs::reassign_face_new_person_uses_numeric_timestamp, ::reassign_face_cleans_empty_unnamed_source_but_preserves_anchored_verdicts (the mapping agent missed these; verified present). Was: bulk.rs handle_reassign_face new-person path and source cleanup untested. Add to commands/bulk.rs tests. |
| ReassignFaceTests | refusesMissingFaceStaleDestinationAndConflictingPayload | Reassign is refused for a missing face, stale destination or contradictory payload. | a (closed round 14; was d) | platforms/windows/src/engine/src/commands/bulk.rs::reassign_face_rejects_missing_face_and_stale_destination, ::reassign_face_rejects_conflicting_destination_payload (the mapping agent missed these; verified present). Was: Error branches of handle_reassign_face (bulk.rs ~2011-2081) untested. Add to commands/bulk.rs tests. |
| ReassignFaceTests | revertMergeRestoresTheMergedFacesUnderTheOldPerson | Undoing a merge puts faces back under the old person. | a | commands/person_merge_tests.rs::undo_restores_names_faces_counts_and_verdicts (Round 17 second pass: failed with the blank-target name check in undo removed.) |

### StablePersonAssignmentsTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| StablePersonAssignmentsTests | sharedFixtures | Stable person ids follow the shared overlap fixtures. | a | commands/face_clustering_mac_tests.rs::stable_ids_go_to_the_largest_overlap + commands/face_clustering.rs::overlap_inheritance_requires_half_of_prior_faces_rounded_up, one_prior_identity_has_one_deterministic_inheritance_winner |
| StablePersonAssignmentsTests | sharedProtectedPartitions | Protected persons keep their partition in the shared fixtures. | a | commands/face_clustering.rs::protected_owner_outside_pool_or_without_faces_is_preserved + manual_merge_* tests |

### PersonCountsRecountTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| PersonCountsRecountTests | recountStaleCorrectsWrongExcludedAndEmptyCounts | Stale person file counts are corrected, including excluded/empty. | a | db/person_counts.rs::recount_stale_corrects_wrong_excluded_and_empty_counts |
| PersonCountsRecountTests | recountStaleLeavesCorrectRowsAlone | Correct counts are not rewritten. | a | db/person_counts.rs::recount_stale_leaves_correct_rows_alone |
| PersonCountsRecountTests | trashingAFileRecountsItsPersons | Trashing a file recounts the people in it. | a | db/person_counts.rs::reconcile_recounts_only_the_named_persons (the trash caller itself is only reached via conformance files tests) |

### CatalogTakeAutoStartTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogTakeAutoStartTests | idleLibraryStartsTheJobOnceAndItCompletes | An idle library starts take grouping once after a scan and it finishes. | a | commands/take_grouping_tests.rs::the_job_starts_by_itself_once_everything_else_is_idle + conformance takes-grouping-starts-after-a-scan-with-new-pictures |
| CatalogTakeAutoStartTests | aBusyCatalogJobKeepsItFromStarting | Another running or queued catalog job blocks the auto start. | a | commands/take_grouping_tests.rs::another_catalog_job_that_is_running_or_queued_counts_as_busy |
| CatalogTakeAutoStartTests | aPausedOrRunningJobIsLeftAlone | A paused or running grouping job is not restarted. | a | commands/take_grouping_tests.rs::only_a_finished_job_may_start_again_after_a_scan |
| CatalogTakeAutoStartTests | theScanCoordinatorCountsOnlyAnalysedMedia | Only newly analysed pictures/videos trigger grouping. | a | conformance takes-grouping-starts-after-a-scan-with-new-pictures |

### CatalogTakeGroupingTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogTakeGroupingTests | groupsNeedSimilarContentAndNameTheRelation | Takes group only similar pictures and name their relation. | a | commands/take_grouping_tests.rs::groups_need_similar_content_and_name_the_relation |
| CatalogTakeGroupingTests | rerunsAreIdempotentAndOnlyTouchChangedSessions | Re-running changes only changed sessions. | a | commands/take_grouping_tests.rs::reruns_are_idempotent_and_only_touch_changed_sessions |
| CatalogTakeGroupingTests | userChoicesEventsAndPeopleAreRespected | User choices, events and people are kept. | a | commands/take_grouping_tests.rs::user_choices_events_and_people_are_respected |
| CatalogTakeGroupingTests | interruptedJobsPauseAndResumeWithoutRedoingSessions | An interrupted grouping job resumes without redoing work. | a | commands/take_grouping_tests.rs::interrupted_jobs_pause_and_resume_without_redoing_sessions + conformance takes-grouping-resumes-after-interruption |
| CatalogTakeGroupingTests | largeFilesWithOneSampledHashStayTwoTriesWhileSmallCopiesCollapse | Large files sharing a sampled hash stay separate; small exact copies collapse. | a | commands/take_grouping_tests.rs::large_files_with_one_sampled_hash_stay_two_tries_while_small_copies_collapse |

### CatalogTakesTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogTakesTests | outcomePrecedesQualityAndUnknownTakesAbstain | A reviewed outcome outranks quality; unknown outcomes abstain. | a | commands/catalog_takes.rs::reviewed_outcome_precedes_quality_and_undo_restores_abstention |
| CatalogTakesTests | groupFeedbackUndoAndSourceChange | Keep-all feedback undoes and resets when a source changes. | a | conformance catalog-events-takes-and-chapters-edit-and-undo + commands/catalog_takes.rs::changed_goal_invalidates_prior_outcomes_and_preference_until_review_or_undo |

### CatalogTakeSuggestionsTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogTakeSuggestionsTests | groupsCurrentVisualEvidenceWithoutChangingEvents | Suggestions group by current visual evidence and do not alter events. | a | commands/catalog_take_suggestions.rs::visual_grouping_skips_exact_duplicates_derived_versions_and_distant_takes |
| CatalogTakeSuggestionsTests | largeFilesWithOneSampledHashAreDifferentTriesSmallCopiesAreOne | Same as above for suggestions. | a | commands/catalog_take_suggestions.rs::large_files_with_one_sampled_hash_are_different_tries_small_copies_are_one |

### NearDuplicateGroupingTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| NearDuplicateGroupingTests | groupsABurstAndItsRawSidecarsButNotTheNextShot | A burst plus its RAW sidecars group; the next shot does not. | a | pipeline/near_duplicate_grouping.rs::groups_a_burst_and_raw_sidecars_but_not_the_next_shot |
| NearDuplicateGroupingTests | aLaterExportOfTheRawJoinsItsShot | A later JPEG export of a RAW joins its shot. | a | pipeline/near_duplicate_grouping.rs::a_later_export_of_the_raw_joins_its_shot |
| NearDuplicateGroupingTests | timeGapsAndMissingTimesNeverJoin | Long gaps or missing times never join. | a | pipeline/near_duplicate_grouping.rs::time_gaps_and_missing_times_never_join |
| NearDuplicateGroupingTests | bestTakesGroupsOnPHashAloneButDescribeDoesNot | Best Takes may group on perceptual hash only; Describe may not. | a | pipeline/near_duplicate_grouping.rs::best_takes_groups_on_phash_alone_but_describe_does_not |
| NearDuplicateGroupingTests | missingEmbeddingsKeepFilesSeparateExceptSidecarPairs | Files without embeddings stay apart except sidecar pairs. | a | pipeline/near_duplicate_grouping.rs::missing_embeddings_keep_files_separate_except_sidecars |
| NearDuplicateGroupingTests | driftFromTheFirstFrameSplitsALongPan | Drift from the first frame splits a long pan. | a | pipeline/near_duplicate_grouping.rs::drift_from_the_first_frame_splits_a_long_pan |
| NearDuplicateGroupingTests | sequenceSuffixesNumberShotsAndShareANumberAcrossSidecars | Shots get sequence numbers shared by sidecars. | a | pipeline/near_duplicate_grouping.rs::sequence_suffixes_match_macos |
| NearDuplicateGroupingTests | appendingSequenceKeepsTheSixtyCharacterBudget | Adding a sequence suffix keeps names within 60 characters. | a (corrected round 17; was d) | platforms/windows/src/engine/src/pipeline/near_duplicate_grouping.rs::tests::sequence_suffixes_match_macos (its last two lines assert the 60-character budget and the suffix; failed with the budget made 70) |
| NearDuplicateGroupingTests | embeddingBlobDecodesLittleEndianFloats | Embedding blobs decode as little-endian floats. | a | pipeline/near_duplicate_grouping.rs::embedding_blob_decodes_little_endian |
| NearDuplicateGroupingTests | cutsScenesOnEmbeddingJumpsAndPicksAMemberRepresentative | Video scenes cut on embedding jumps; the representative is a member frame. | a | pipeline/video_scene_selection.rs::cuts_scenes_on_embedding_jumps |
| NearDuplicateGroupingTests | cutsBetweenShotsOfOneSubjectButNotWithinAHandheldShot | Cuts happen between shots but not inside a handheld one. | a | pipeline/video_scene_selection.rs::cuts_between_shots_of_one_subject_but_not_within_a_handheld_shot |
| NearDuplicateGroupingTests | ignoresAFlashShorterThanTheMinimumScene | A brief flash is not a scene. | a | pipeline/video_scene_selection.rs::ignores_a_short_flash |
| NearDuplicateGroupingTests | activityWindowsCoverOnlyMotionAndRespectFrameLimits | Activity windows cover motion within frame limits. | a | pipeline/video_scene_selection.rs::activity_windows_cover_only_motion |
| NearDuplicateGroupingTests | longActivitySplitsIntoBoundedWindows | Long activity splits into bounded windows. | a | pipeline/video_scene_selection.rs::long_activity_splits_into_bounded_windows |
| NearDuplicateGroupingTests | shortWindowsSendEveryPoolFrame | Short windows send every pool frame. | a | pipeline/video_scene_selection.rs::short_windows_send_every_pool_frame |
| NearDuplicateGroupingTests | poolTimesStayInsideTheVideo | Sample times stay inside the video. | a | pipeline/video_scene_selection.rs::pool_times_stay_inside_the_video |
| NearDuplicateGroupingTests | readsDescriptionFilenameAndTagsFromOneReply | One model reply yields description, filename and tags. | a | pipeline/deep_analyze_naming.rs::combined_visual_completion_parses_all_labeled_outputs |
| NearDuplicateGroupingTests | aReplyWithoutTagsLeavesTheSecondPassAvailable | A reply with no TAGS line still allows the separate tag pass. | a (closed round 17; was d) | platforms/windows/src/engine/src/pipeline/deep_analyze_naming.rs::tests::a_reply_without_tags_keeps_its_other_sections_and_leaves_tags_to_the_fallback (no TAGS: description and name parse, tags empty so `visual_answers` runs its tag fallback; failed with FILENAME dropped from the description end labels) |
| NearDuplicateGroupingTests | systemPromptAsksForTagsOnlyWhenMerged | The prompt requests TAGS only in merged mode. | a (closed round 17; was d; the shape differs) | platforms/windows/src/engine/src/pipeline/deep_analyze_naming.rs::tests::a_reply_without_tags_keeps_its_other_sections_and_leaves_tags_to_the_fallback (the Rust combined prompt always asks for TAGS and asks for FILENAME only when renames are requested; the Swift "merged" switch does not exist) |

### TakeScoringTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TakeScoringTests | theSharpTryWinsAndSaysWhy | The sharper try wins with a reason chip. | a | pipeline/take_scoring.rs::the_sharp_try_wins_and_says_why + conformance takes-score-sharp-try-and-reasons |
| TakeScoringTests | identicalTriesGetNoChip | Identical tries get no recommendation chip. | a | pipeline/take_scoring.rs::identical_tries_get_no_chip |
| TakeScoringTests | sharedProblemsAreNotReasonsAndClosedEyesLose | Problems all tries share are not reasons; closed eyes lose. | a | pipeline/take_scoring.rs::shared_problems_are_not_reasons_and_closed_eyes_lose |
| TakeScoringTests | biggerSubjectAndEveryoneInShotWinWhenSharpnessTies | With equal sharpness, the bigger subject and fuller group win. | a | pipeline/take_scoring.rs::bigger_subject_and_everyone_in_shot_win_when_sharpness_ties |
| TakeScoringTests | cameraShiftIsFoundAndResidualIsSmall | Camera shift is estimated and the residual is small. | a | pipeline/take_scoring.rs::camera_shift_is_found_and_residual_is_small |
| TakeScoringTests | signalsRoundTripThroughTheCacheJSON | Scoring signals survive the cache JSON. | a | pipeline/take_scoring.rs::signals_round_trip_through_the_cache_json |
| TakeScoringTests | eyeOpennessFollowsTheEyeOutline | Openness follows the eye outline. | a | models/face_mesh.rs::eyelid_gap_over_width_scores_openness |
| TakeScoringTests | byteIdenticalCopiesAreNotTakes | Exact copies are not alternative takes. | a | commands/catalog_take_auto.rs::byte_identical_copies_are_not_takes |
| TakeScoringTests | aGroupOfThreeWithTwoCopiesShowsTwoTriesAndKeepsTheLowestID | Two copies in a trio show as one try and keep the lowest id. | a | commands/catalog_take_auto.rs::a_group_of_three_with_two_copies_shows_two_tries_and_keeps_the_lowest_id |
| TakeScoringTests | filesTooLargeForAFullHashAndFilesWithoutAHashAreNeverCollapsed | Unhashable or oversized files are never treated as copies. | a | commands/catalog_take_auto.rs::files_too_large_for_a_full_hash_and_files_without_a_hash_are_never_collapsed (Round 17 second pass: failed with the full-hash size cap, and separately the no-hash rule removed.) |
| TakeScoringTests | derivedFilesAndEventMembersAreLeftOutOfSuggestions | Derived files and event members are not suggested. | a | commands/catalog_take_auto.rs::derived_files_and_event_members_are_left_out_of_suggestions |
| TakeScoringTests | burstsGroupIntoShotsWithRawTwinsAndChoicesHideThem | Bursts become shots with RAW twins; choices hide them. | a | commands/catalog_take_auto.rs::bursts_group_into_shots_with_raw_twins_and_choices_hide_them |
| TakeScoringTests | scoringReadsPNGTriesAndCachesByRevision | PNG tries are scored and cached per source revision. | a | commands/catalog_take_auto.rs::scoring_reads_png_tries_and_caches_by_revision |
| TakeScoringTests | goalRepliesAreValidatedAgainstTheSheet | Goal replies are checked against the contact sheet numbers. | a | commands/take_goal.rs::replies_parse_and_stay_in_range + contact_sheet_numbers_every_tile |
| TakeScoringTests | goalChecksWakeTheBestInstalledVisionModel | The goal check picks the best installed vision model (not a text model). | d | Rust take_goal.rs only says 'unavailable' without a model; no model-choice test. Add to commands/take_goal.rs. |
| TakeScoringTests | trashJournalSnapshotsAndRestoresCatalogRows | Trash snapshots catalog rows and restores them exactly. | a | commands/trash.rs::restored_catalog_reconciliation_is_exact_and_idempotent (Round 17 second pass: failed with the exact-row check on an ignored insert removed.) |

### TakeOutcomeAnalysisTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TakeOutcomeAnalysisTests | parsesGoalEvidenceAndHiddenOutcomeAbstention | Model goal evidence parses; hidden outcomes make a take abstain. | d | Rust has take_goal.rs reply parsing for the contact-sheet ranking but no per-clip outcome-evidence parser tests (and no clip goal-evidence pipeline found). Add to commands/take_goal.rs or a new pipeline test if the feature is kept. |
| TakeOutcomeAnalysisTests | rejectsInvalidAndUnboundedModelResponses | Out-of-range or oversized model replies are rejected. | a | commands/take_goal.rs::replies_parse_and_stay_in_range (contact-sheet replies only; clip evidence unverified) |
| TakeOutcomeAnalysisTests | failureRequiresEveryWindowAndWholeClipSuccessRequiresCompleteEvidence | A clip fails only if every window fails; success needs complete evidence. | d | No Rust counterpart test. Add alongside the clip outcome code. |
| TakeOutcomeAnalysisTests | timestampMappingRejectsDuplicateOrOutOfWindowFrames | Frame timestamps outside or repeated in a window are rejected. | d | timeline_analysis_tests distinct_frames_drop_repeated_timestamps is for timeline moments, not take outcomes. |
| TakeOutcomeAnalysisTests | automaticScoresReachRankingWithResumeAndPreserveUserCorrections | Automatic outcome scores feed ranking, resume after pause, and keep user corrections. | d | No Rust test of auto outcome scores surviving user corrections. Add to commands/catalog_takes.rs. |
| TakeOutcomeAnalysisTests | goalEditsSourceChangesAndPauseRefuseLateEvidence | Late evidence after a goal edit, source change or pause is refused. | d | Not tested in Rust. Add to commands/catalog_takes.rs. |
| TakeOutcomeAnalysisTests | editingTheDesiredGoalInvalidatesPreviousReviewsAndUndoRestoresThem | Changing the goal invalidates old reviews; undo restores them. | a | commands/catalog_takes.rs::changed_goal_invalidates_prior_outcomes_and_preference_until_review_or_undo |
| TakeOutcomeAnalysisTests | each window time takes its nearest pool frame, once, within one pool step | Window times map to distinct nearest pool frames. | d | video_scene_selection pool tests do not assert nearest-unique mapping. Add to pipeline/video_scene_selection.rs. |

### TakeFaceTilesTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TakeFaceTilesTests | sharedTilesTriggersPlacementAndMerge | Face tile layout, triggers and merging match the shared fixture. | a | pipeline/take_scoring.rs::shared_small_face_tiles_match_swift + commands/catalog_take_auto.rs::crops_copy_the_tile_rows |
| TakeFaceTilesTests | faceSizesAreJudgedAtTheTileScale | Face size is judged at tile scale. | a | commands/catalog_take_auto.rs::a_wide_frame_without_faces_is_tiled_and_judged_at_the_tile_scale + face_regions_match_the_photo_resized_to_the_tile_size |

### CatalogStoreTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogStoreTests | chapterUndoSurvivesReopenAndKeepsChangedSourcesStale | Chapter undo survives restart and changed sources stay stale. | a | commands/catalog.rs::chapter_evidence_is_searchable_and_stale_after_source_change + conformance catalog-events-takes-and-chapters-edit-and-undo |
| CatalogStoreTests | storesManualChaptersAndFindsTimestampEvidence | Hand-made chapters are stored and found as timestamp evidence. | a | commands/catalog.rs::chapter_evidence_is_searchable_and_stale_after_source_change |
| CatalogStoreTests | rejectsCrossFileChapterReplacementAndInvalidTiming | A chapter can't replace another file's chapter and bad times are refused. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/catalog.rs::tests::a_chapter_cannot_replace_another_files_chapter_and_bad_times_are_refused (failed with the owner check removed, with the finite check removed, and with the title trim removed; negative and reversed times are also refused by the table CHECKs) |

### CatalogCastTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogCastTests | detectedAndReviewedAppearancesKeepTheirEvidence | A video lists a person as Detected (face) and Reviewed (timestamped) with evidence ids. | d | No Rust test of person appearance hits. Add to commands/catalog_retrieval.rs. |
| CatalogCastTests | staleIgnoredOrAmbiguousIdentitiesAreAbsent | Excluded, Unknown, wrong-model, wrong-revision, changed-source or generic people never show as cast. | d | Same; parameterised cases excluded/unknown/revision/model/source/generic. Add to commands/catalog_retrieval.rs. |

### CatalogCaptureWindowTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogCaptureWindowTests | windowKeepsFilesCapturedInsideIt | A date window keeps only files shot inside it. | a | catalog_index_tests.rs::capture_window_keeps_files_inside_it_and_rejects_backwards_ranges |
| CatalogCaptureWindowTests | windowRespectsTheRequestedLimitAndRejectsBackwardsRanges | The limit and backward ranges are honoured. | a | catalog_index_tests.rs::capture_window_keeps_files_inside_it_and_rejects_backwards_ranges |
| CatalogCaptureWindowTests | hybridWithoutAVectorExplainsTheMissingModel | Hybrid search without a vector says the model is missing. | a | catalog_index_tests.rs::cold_responses_match_schema_and_do_not_claim_similarity_completion (weakly); explicit message not asserted |
| CatalogCaptureWindowTests | a dated search reaches files beyond the hundred best matches | A dated search is not limited to the top 100 text hits. | a | catalog_index_tests.rs::capture_window_reaches_files_beyond_the_ranked_hundred |

### CatalogNameProposalsTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogNameProposalsTests | confirmedTokensCollisionsAndValidation | Confirmed words, collisions and validation behave. | a | commands/catalog_names.rs::confirmed_tokens_collisions_and_validation + conformance names-styles-budget-and-confirmed-tokens |
| CatalogNameProposalsTests | companionsShareOneStemAndBumpTogether | RAW/JPEG/sidecar companions share a stem and bump together. | a | commands/catalog_names.rs::companions_share_one_stem_and_bump_together + util/smart_name_companions.rs::shared_names_bump_together_or_not_at_all |
| CatalogNameProposalsTests | learnedStyleIsRecordedAppliedAndReset | Learned naming style is recorded, applied and reset. | a | commands/catalog_names.rs::learned_style_is_recorded_applied_and_reset |
| CatalogNameProposalsTests | undoneRenamesForgetTheirNewestMatchingSample | Undoing a rename forgets its learned sample. | a | commands/catalog_names.rs::undone_renames_forget_their_newest_matching_sample |

### CatalogNamingContextTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogNamingContextTests | reviewedTitleProducesShortSpecificName | A reviewed title gives a short specific name. | a | pipeline/naming_context.rs::reviewed_title_produces_a_short_specific_name |
| CatalogNamingContextTests | draft/stale/generic/conflict/goal | Draft, stale, generic, conflicting or goal-only evidence abstains. | a | pipeline/naming_context.rs::ambiguous_or_unreviewed_evidence_abstains + a_label_that_only_repeats_the_file_name_abstains + late_evidence_changes_fail_validation |
| CatalogNamingContextTests | source/label/person | Changed source, missing label or ambiguous person blocks naming. | a | pipeline/naming_context.rs::a_changed_or_missing_source_fails_the_load + title_and_first_name_form_the_subject |
| CatalogNamingContextTests | excluded/unknown/model/revision/failed/generic identity | Unusable identities are omitted from naming hints. | a | pipeline/naming_context.rs::invalid_identity_hints_are_omitted + identity_without_a_selected_model_is_not_used + valid_identity_evidence_is_used |

### CatalogTextSearchTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogTextSearchTests | chatAndCatalogFindExtractedDocumentAndOCRText | Extracted document text and OCR text are searchable. | a | conformance search-text-hits-good-matches-first-weaker-counted + catalog-search-labels-handwritten-text; read/text.rs tests |
| CatalogTextSearchTests | chatFindsScannedTagsWithoutDescriptions | Chat finds files by scanned tags when no description exists. | a | commands/catalog_retrieval.rs::scanned_tags_answer_without_descriptions |
| CatalogTextSearchTests | pluralWordForms | Plural query words reach singular tags. | a | commands/catalog_retrieval.rs::plural_words_add_singular_stems + conformance catalog-search-plural-words-reach-singular-tags |
| CatalogTextSearchTests | evidencePromptIsNumberedAndBounded | The chat evidence prompt numbers hits and is size-bounded. | a (corrected round 17; was d) | platforms/windows/src/engine/src/commands/chat.rs::tests::evidence_prompt_is_numbered_and_bounded (failed with the limit removed) |

### CatalogVectorIndexTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CatalogVectorIndexTests | cache restart and incremental replacement preserve entity mappings | Restart plus incremental updates keep file-to-vector mapping. | a | catalog_index_tests.rs::small_changes_ride_beside_the_graph_and_survive_a_restart |
| CatalogVectorIndexTests | corrupt caches and missing change history rebuild from SQLite | Damaged cache or missing history rebuilds from the database. | a | catalog_index_tests.rs::graph_roundtrip_is_restored_and_corruption_rebuilds + delta_gap_rebuilds_invalid_vectors_are_excluded_and_snapshots_are_bounded |
| CatalogVectorIndexTests | changed files invalidate derived vectors but preserve user evidence | A changed file drops its vector but keeps user data. | a | catalog_index_tests.rs::semantic_hybrid_scope_exclusions_and_changed_sources_are_real |
| CatalogVectorIndexTests | capacity limits and protected cache paths fail without changing source rows | Size caps and unwritable cache paths fail safely. | a | catalog_index_tests.rs::a_library_past_eighty_thousand_photos_round_trips_within_the_size_limit + truncated_and_damaged_binary_snapshots_are_refused_without_panicking (protected-path case not tested) |
| CatalogVectorIndexTests | a divergent database with the same generation rebuilds the graph | A different database with equal generation number rebuilds. | a | catalog_index_tests.rs::incompatible_model_nonce_dimension_and_topology_never_restore |
| CatalogVectorIndexTests | rolled back embedding writes do not advance the index checkpoint | A rolled-back write is not counted as indexed. | a (closed round 17; was d) | platforms/windows/src/engine/src/catalog_index_tests.rs::a_rolled_back_vector_write_does_not_advance_the_index_checkpoint (also: a reader never sees the uncommitted revision, and the next committed write is one step on, beside the graph). The revision lives in SQL triggers that roll back with the write, so there is no engine guard to break; the test pins the outcome |
| CatalogVectorIndexTests | hybrid retrieval keeps timestamp evidence and excludes failed files | Hybrid results keep timestamp hits and drop failed files. | a | catalog_index_tests.rs::semantic_hybrid_scope_exclusions_and_changed_sources_are_real + commands/catalog_retrieval.rs::filters_apply_before_limits_and_exclude_failed_stale_and_unknown |
| CatalogVectorIndexTests | semantic requests reject incompatible vectors and malformed filters | Wrong-size vectors and bad filters are refused. | a | commands/catalog_search.rs::strict_vector_model_seed_modes_limits_and_scope |
| CatalogVectorIndexTests | failed vectors cannot crowd out eligible files | Failed vectors never take result slots. | a | commands/catalog_retrieval.rs::filters_apply_before_limits_and_exclude_failed_stale_and_unknown |
| CatalogVectorIndexTests | matching moments cannot crowd other files out of a Library result | Many matches in one video don't push out other files. | d | No Rust test limits per-file moments in a Library result. Add to catalog_index_tests.rs or commands/catalog_search.rs. |
| CatalogVectorIndexTests | concurrent refreshed queries retain complete graph ownership | Queries during an index refresh see a whole graph. | d | catalog_index uses Arc<HnswMap>; no concurrent-refresh test. Add to catalog_index_tests.rs. |
| CatalogVectorIndexTests | background indexing defers to a resident model and resumes after it unloads | Indexing waits for a resident model and resumes. | a | catalog_index_tests.rs::admission_defers_durably_without_false_completion_or_keyword_loss |
| CatalogVectorIndexTests | memory deferral and restart recovery keep an explicit resumable job | A memory-deferred job survives restart as resumable. | a | catalog_index_tests.rs::retained_memory_refresh_defers_then_cancel_resume_can_release_and_rebuild + coalesced_jobs_pause_resume_cancel_and_restart_without_writer_lock |
| CatalogVectorIndexTests | cancelling a partial update preserves the verified snapshot and permits explicit retry | Cancelling keeps the good snapshot and can be retried. | a | catalog_index_tests.rs::coalesced_jobs_pause_resume_cancel_and_restart_without_writer_lock + a_failed_save_keeps_search_working_and_is_retried |
| CatalogVectorIndexTests | failed index jobs retry without a visual model and keyword search survives | Keyword search keeps working when the index job fails. | a | catalog_index_tests.rs::admission_defers_durably_without_false_completion_or_keyword_loss |
| CatalogVectorIndexTests | memory admission includes an oversized historical cache even when live rows are few | A huge old cache counts against memory admission. | a | catalog_index_tests.rs::invalid_cache_candidates_are_rejected_before_constrained_memory_admission + bounded_corrupt_graph_with_compatible_manifest_rebuilds_under_constrained_memory |

### HNSWRecallTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| HNSWRecallTests | dense clusters retain routes to exact nearest neighbours | In dense clusters the graph search still returns the exact nearest neighbours. | a (closed round 17; was d) | platforms/windows/src/engine/src/util/hnsw_index.rs::tests::dense_clusters_keep_routes_to_the_exact_nearest_neighbours (16,000 vectors in 64 clusters, 20 queries, worst recall >= 0.9 and mean >= 0.98, as Swift; the search index Rust uses; failed (mean 0.07) with ef_search 1 and ef_construction 4) |

### HNSWSnapshotTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| HNSWSnapshotTests | restoration preserves graph, tombstones, search and incremental insertion | Swift HNSW snapshot restore keeps graph and tombstones. | c | Swift HNSWIndex snapshot format; Rust rebuilds from the change log (SWIFT_ENGINE_RETIREMENT 2.4 'by design'); related Rust pin: catalog_index_tests.rs::graph_roundtrip_is_restored_and_corruption_rebuilds |
| HNSWSnapshotTests | empty graphs persist and incompatible or stale snapshots are rejected | Empty graphs persist; wrong-model snapshots rejected. | a | catalog_index_tests.rs::incompatible_model_nonce_dimension_and_topology_never_restore |
| HNSWSnapshotTests | corruption, truncation and bounded malformed fields are rejected | Damaged snapshots are refused, never crash. | a | catalog_index_tests.rs::truncated_and_damaged_binary_snapshots_are_refused_without_panicking |
| HNSWSnapshotTests | atomic disk snapshots reject protected destinations | Snapshot writes are atomic and refuse protected paths. | c | Swift snapshot writer; Rust format differs |
| HNSWSnapshotTests | compaction and snapshot processing honor cancellation | Compaction/snapshot stop on cancel. | c | Swift-only compaction; Rust has no compaction |

### SearchStrengthTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| SearchStrengthTests | baselineMatchesTheSharedFile | The baseline score matches the shared file. | a | models/search_strength.rs::baseline_is_the_current_model_and_unit_length |
| SearchStrengthTests | sharedCasesRankAndJudge | Shared cases rank and judge Good/weaker identically. | a | models/search_strength.rs::shared_cases_rank_and_judge_like_swift |
| SearchStrengthTests | sharedWording | 'N good matches' wording matches the shared file. | a | models/search_strength.rs::shared_summaries_and_similar_wording |
| SearchStrengthTests | scoringFallsBackToTheClientVector | Scoring uses the client's vector when there is no stored one. | a | models/search_strength.rs::scoring_falls_back_to_the_client_vector |
| SearchStrengthTests | text search puts good matches first, counts weaker ones, and keyword evidence makes a file good | Good matches first, weaker counted, keyword hit counts as good. | a | catalog_index_tests.rs::text_search_puts_good_matches_first_and_keyword_evidence_makes_a_file_good + conformance search-text-hits-good-matches-first-weaker-counted |

### BGEDocumentMatrixTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| BGEDocumentMatrixTests | cached scoring returns exactly what the row scan returns, and follows new rows | The cached document-similarity matrix equals a full scan and tracks new rows. | a | commands/chat.rs::cached_document_similarity_matches_the_scan_and_follows_every_change |

### BGETextServiceTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| BGETextServiceTests | loads + embeds: same-topic docs are closer than different-topic | The BGE text embedder separates topics. | c | Needs the real BGE weights; Swift BGETextService disappears with the engine. Rust path is commands/chat.rs bge_hits; no model-level test on either side. |

### WordPieceTokenizerTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| WordPieceTokenizerTests | wraps with [CLS]/[SEP], mask all 1, types all 0 | Tokens are wrapped with CLS/SEP. | a | models/wordpiece_tokenizer.rs::wraps_with_cls_and_sep |
| WordPieceTokenizerTests | greedy splits into wordpieces | playing becomes play ##ing. | a | models/wordpiece_tokenizer.rs::greedy_splits_into_wordpieces |
| WordPieceTokenizerTests | unknown word becomes [UNK] | Unknown words become UNK. | a | models/wordpiece_tokenizer.rs::unknown_word_becomes_unk |
| WordPieceTokenizerTests | lowercases and splits punctuation | Case folded, punctuation split. | a | models/wordpiece_tokenizer.rs::lowercases_and_splits_punctuation |
| WordPieceTokenizerTests | truncates to maxLen keeping [CLS]/[SEP] | Truncation keeps both special tokens. | a | models/wordpiece_tokenizer.rs::truncates_to_max_len_keeping_cls_sep |
| WordPieceTokenizerTests | missing special tokens fails init | A vocabulary without special tokens is refused. | a | models/wordpiece_tokenizer.rs::missing_special_tokens_is_an_error |
| WordPieceTokenizerTests | all-punctuation splits per char | Punctuation-only text splits per character. | a | models/wordpiece_tokenizer.rs::all_punctuation_input_splits_per_char |
| WordPieceTokenizerTests | word past max input chars becomes [UNK] | Over-long words become UNK. | a | models/wordpiece_tokenizer.rs::word_past_max_input_chars_becomes_unk |

### DeepAnalyzeTests (DeepAnalyzeTests.swift (6 suites, 34 tests; no struct named DeepAnalyzeTests))

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| DeepAnalyzeTests | Qwen3VLWeightAdapter: already-normalized lm_head keys survive | MLX weight key sanitiser keeps normalized keys. | c | Shared FileIDVLMCore/adapter code, stays (moves to SharedTests) |
| DeepAnalyzeTests | Qwen3VLWeightAdapter: unrelated model keys remain unchanged | Other keys untouched. | c | Same, stays |
| DeepAnalyzeTests | LocalVLMDownloader: returns only the verified repository directory | The offline factory returns only a verified model dir. | c | Shared VLM code, stays |
| DeepAnalyzeTests | LocalVLMDownloader: offline catalog attestations require audited weight hashes | Substituted weights are refused. | a | models/mlx_tests.rs::catalog_models_must_carry_the_audited_weights |
| DeepAnalyzeTests | filename prompt forbids inferred dates | The rename prompt forbids invented dates. | a | models/vlm.rs::rename_prompt_forbids_invented_dates |
| DeepAnalyzeTests | malformed generated filenames are repaired or rejected | Bad generated names are fixed or dropped. | a | pipeline/deep_analyze_naming.rs::malformed_generated_filenames_are_repaired_or_rejected |
| DeepAnalyzeTests | empty metadata-only results never establish model completion | A result with no model output is a failure, not 'done'. | a (corrected round 17; was d) | platforms/windows/src/engine/src/apple/vlm.rs::tests::analysis_failures_match_the_swift_runner (the same predicate on the Mac path; `apple-adapters` build only, not re-run in round 17) |
| DeepAnalyzeTests | unsupported identity claims and filename names are removed | Claims of who is in a photo without evidence are stripped. | a | pipeline/deep_analyze_naming.rs::document_text_and_visual_claim_filter_are_bounded |
| DeepAnalyzeTests | known face names remain available to the model | Names of recognised people are passed to the model. | a | models/vlm.rs caption_prompt_with_faces (no direct test); partial: pipeline/deep_analyze.rs::a_reviewed_label_leaves_the_name_out_of_a_document_request |
| DeepAnalyzeTests | proposed filenames retain only trusted metadata or OCR years | Years in names must come from capture date, OCR or tags. | a | pipeline/deep_analyze_naming.rs::proposed_filename_years_must_be_trusted |
| DeepAnalyzeTests | parseVLMTags: splits, lowercases, strips punctuation | Tag parsing basics. | a | pipeline/deep_analyze.rs::parse_vlm_tags_splits_lowercases_and_strips_punct |
| DeepAnalyzeTests | parseVLMTags: strips list numbering and dedupes | Numbering stripped, duplicates dropped. | a | pipeline/deep_analyze.rs::parse_vlm_tags_strips_numbering_and_dedupes |
| DeepAnalyzeTests | parseVLMTags: drops sentence fragments (>2 words) | Long fragments dropped. | a | pipeline/deep_analyze.rs::parse_vlm_tags_drops_sentence_fragments_keeps_short |
| DeepAnalyzeTests | parseVLMTags: filters generic stopwords and caps at 2 | Generic words filtered, max two tags. | a | pipeline/deep_analyze.rs::parse_vlm_tags_drops_generic_tokens + parse_vlm_tags_caps_count |
| DeepAnalyzeTests | ModelLoadGate: shared load cancels only when the final waiter bails | A shared model load is cancelled only by its last waiter. | c | Swift-only ModelLoadGate; Rust uses resident-owner tests in models/resident_owner_tests.rs (different mechanism) |
| DeepAnalyzeTests | pixelsExceedDecodeCap: 50 MP cap | Images over 50 megapixels are not decoded for the model. | a | pipeline/deep_analyze_naming.rs::vlm_pixel_cap_accepts_boundary_and_rejects_oversize |
| DeepAnalyzeTests | video keyframe target is one quarter of duration | The video keyframe is taken at 25% of the duration. | c (round 17; was d) | Swift `DeepAnalyze.representativeVideoTime` in FileIDVLMCore stays (the Mac adapter uses it); the test is ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::DescribeKeyframeTests |
| DeepAnalyzeTests | video duration loading has a wall-clock timeout | Reading a video's length times out instead of hanging. | c | Swift AVFoundation call; Rust shell/video.rs differs (no counterpart test) |
| DeepAnalyzeTests | treeListURL: listing is recursive | Model repo listing is recursive. | c | Swift VLMDownloader helper; Rust: models/mlx_tests.rs::listing_reads_the_hugging_face_tree |
| DeepAnalyzeTests | verified VLM sentinel detects missing and same-size-corrupt files | Install check catches missing or corrupted files. | a | models/registry.rs::runtime_attestation_detects_same_size_corruption_and_missing_leaf + models/mlx_tests.rs::wrong_revision_or_missing_file_is_not_installed |
| DeepAnalyzeTests | escapeLike: backslashes LIKE metacharacters | Folder scope escapes % and _. | a | commands/deep_scope.rs::folder_boundaries_are_literal_and_do_not_exclude_siblings |
| DeepAnalyzeTests | batch proposed names use source stems to avoid duplicates | Batch names stay distinct. | a | pipeline/deep_analyze_naming.rs::reserved_names_get_swift_ordinals_per_folder |
| DeepAnalyzeTests | resolveTargets folder scope: '_' does not over-match siblings | Underscore is literal. | a | commands/deep_scope.rs::folder_boundaries_are_literal_and_do_not_exclude_siblings |
| DeepAnalyzeTests | resolveTargets folder scope: '%' is treated literally | Percent is literal. | a | commands/deep_scope.rs::folder_boundaries_are_literal_and_do_not_exclude_siblings |
| DeepAnalyzeTests | skipExisting keys selected and library scopes on full completion | Skip-existing counts only fully analysed files. | a | commands/deep_analyze.rs::full_pass_skip_is_model_aware |
| DeepAnalyzeTests | resolveTargets wholeLibrary: folder exclusion respects sibling-prefix boundaries | Excluding /a/b does not exclude /a/bc. | a | commands/deep_scope_handler_tests.rs::handler_scope_whole_library_exclusion_uses_directory_boundaries |
| DeepAnalyzeTests | exclusionWhereClause edge cases | Empty/relative exclusions are ignored. | a | commands/deep_scope.rs::whole_library_rejects_malformed_or_unavailable_exclusions |
| DeepAnalyzeTests | persist full pass clears requested empty outputs and stale tags | A full pass replaces old description/tags even when the new output is empty. | a (closed round 17; was d; engine fixed) | platforms/windows/src/engine/src/pipeline/deep_analyze.rs::tests::a_full_pass_replaces_old_model_output_even_when_the_new_output_is_empty_and_keeps_user_tags. Finding fixed: `persist_vlm_results` kept the old caption, name and VLM tags (COALESCE, and no tag replace for an empty list) while stamping the new model; it now replaces what was asked for, as Swift does, and never touches user or scan tags. Failed with the old COALESCE update |
| DeepAnalyzeTests | persist partial clears requested empty output but preserves unrequested same-model data | A partial run changes only the parts asked for. | a (closed round 17; was d; engine fixed) | platforms/windows/src/engine/src/pipeline/deep_analyze.rs::tests::a_pass_changes_only_the_outputs_it_was_asked_for and ::a_reviewed_label_survives_a_full_pass_whose_model_proposed_nothing (the Rust partial pass is Tags only; failed with the tag replace skipped for an empty list) |
| DeepAnalyzeTests | persist tracks full completion without misattributing partial output | Only a complete run is marked fully done. | a | commands/deep_analyze.rs::full_pass_skip_is_model_aware (reads the flag; writing side untested) |
| DeepAnalyzeTests | run(): a cancel issued while queued aborts before load and emits one terminal complete | Cancelling a queued run ends it once without loading the model. | a | commands/deep_scope_admission_tests.rs::scope_admission_cancelled_waiter_never_runs_or_leaks + deep_scope_handler_tests.rs::handler_scope_empty_selection_has_only_terminal_without_model_lookup |
| DeepAnalyzeTests | Trusted years: capture date, OCR text and Year tags ground a year | Trusted years come from the catalog. | a | pipeline/deep_analyze_naming.rs::proposed_filename_years_read_ocr_text_table |
| DeepAnalyzeTests | Prewarm: a Cancel with no prewarm running does not cancel the next | A stray cancel does not stick. | a | commands/prewarm.rs::per_model_cancel_is_isolated |
| DeepAnalyzeTests | Prewarm: a finishing prewarm does not make a concurrent one uncancellable | Concurrent prewarms stay independently cancellable. | a | commands/prewarm.rs::per_model_cancel_is_isolated |

### DeepAnalyzeNamingTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| DeepAnalyzeNamingTests | buildAudioName: Artist - Title, title-only, else nil | Audio names come from artist/title tags. | a | pipeline/deep_analyze_naming.rs::build_audio_name_artist_and_title |
| DeepAnalyzeNamingTests | nameFromTranscript: leading words -> name, lead -> caption | A transcript's first words name the file. | a | pipeline/deep_analyze_naming.rs::name_from_transcript_takes_leading_words |
| DeepAnalyzeNamingTests | nameFromSoundLabel: humanizes events, drops generic labels | Sound labels become readable names. | d | No Rust test for sound-label naming. Add to pipeline/deep_analyze_naming.rs. |
| DeepAnalyzeNamingTests | sound analysis obeys its wall-clock deadline | Sound analysis stops after its time limit. | d | No Rust test. Add to the sound analysis module. |
| DeepAnalyzeNamingTests | buildObjName: meaningful object, else material, else nil | 3D model names prefer object then material. | a | pipeline/deep_analyze_naming.rs::build_obj_name_prefers_meaningful_object_then_material |
| DeepAnalyzeNamingTests | isMeaningfulModelName filters placeholders, keeps real words | Placeholder names like Cube.001 are dropped. | a | pipeline/deep_analyze_naming.rs::is_meaningful_model_name_filters_placeholders |
| DeepAnalyzeNamingTests | parseObjNames reads objects, materials, and the .mtl | OBJ names are read from file and .mtl. | a | pipeline/deep_analyze_naming.rs::parse_obj_names_reads_objects_materials_and_mtl |
| DeepAnalyzeNamingTests | OBJ material libraries stay inside the model folder and must be regular files | An .mtl outside the folder is ignored. | a | pipeline/deep_analyze_naming.rs::parse_obj_names_ignores_mtllib_path_traversal + parse_obj_names_ignores_absolute_mtllib_redirection |
| DeepAnalyzeNamingTests | FileTypes classifies .obj as model and .mp3 as audio | File kinds are classified. | c | FileTypes is shared FileIDModelInfo code and stays; moves to SharedTests |

### VLMSceneCaptionTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| VLMSceneCaptionTests | a block repeated back to back at the tail is found | Greedy decoding detects a repeated token tail. | c | FileIDVLMCore VLMGreedyDecoder, stays (SharedTests) |
| VLMSceneCaptionTests | short or unrepeated text is not a loop | No false loop detection. | c | FileIDVLMCore, stays |
| VLMSceneCaptionTests | a looped reply ends at its last full sentence | A looped caption is trimmed to the last sentence. | c | FileIDVLMCore, stays |
| VLMSceneCaptionTests | the first closed JSON object completes a reply | Decoding stops at the first complete JSON object. | c | FileIDVLMCore, stays |
| VLMSceneCaptionTests | the scene prompt asks for a plain caption without filename or tag lines | The caption prompt has no filename/tag lines. | c | Swift DeepAnalyze prompt; Rust prompt in models/vlm.rs (no equal test) - disappears with engine |
| VLMSceneCaptionTests | scene token cap defaults to 128 and accepts a bounded override | Scene token limit. | c | Swift DeepAnalyze env; disappears with engine |
| VLMSceneCaptionTests | loop guard defaults on and switches off with 0 | Loop guard env switch. | c | FileIDVLMCore, stays |
| VLMSceneCaptionTests | Qwen3-VL 2B defaults to one-line reply, other models to sections | Reply format by model. | c | Swift DeepAnalyze; disappears with engine |
| VLMSceneCaptionTests | captions use the library stream except for the compact-line model | Which captions use the greedy stream. | c | Swift DeepAnalyze; disappears |
| VLMSceneCaptionTests | the compact prompt asks for one line of three fields | Compact prompt shape. | c | Swift DeepAnalyze; disappears |
| VLMSceneCaptionTests | a compact reply parses into description, filename and tags | Compact reply parsing. | c | Swift DeepAnalyze; Rust uses labeled DESCRIPTION/TAGS/FILENAME replies instead |
| VLMSceneCaptionTests | labelled or partial compact replies still parse | Tolerant compact parsing. | c | Swift DeepAnalyze; disappears |
| VLMSceneCaptionTests | a compact reply is complete once its line holds every field | Early stop for compact replies. | c | Swift DeepAnalyze; disappears |

### VLMSentinelIdentityTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| VLMSentinelIdentityTests | installing records every file's identity and an unchanged install skips the hash | The install record stores file identity so unchanged installs are not re-hashed. | a | models/mlx_tests.rs::plan_keeps_verified_files_and_finish_writes_the_rust_record + conformance mlx-verified-install.json |
| VLMSentinelIdentityTests | a recorded size, mtime, ctime, inode, device or manifest that differs forces a re-hash | Any changed identity field forces a re-hash. | a | models/mlx_tests.rs (unix tests near lines 233-310 on device/inode/ctime) |
| VLMSentinelIdentityTests | touching or replacing a file on disk forces a re-hash | A touched or replaced file is re-verified. | a | models/mlx_tests.rs unix identity tests |
| VLMSentinelIdentityTests | a swapped same-size file with its old mtime fails verification | A same-size swapped file is caught. | a | models/registry.rs::runtime_attestation_detects_same_size_corruption_and_missing_leaf + mlx_tests.rs same-tick test |
| VLMSentinelIdentityTests | a version-1 sentinel alone is re-hashed once, then upgraded | Old sentinels upgrade after one hash. | a | models/mlx_tests.rs::swift_record_is_accepted_and_gains_an_identity_record + conformance mlx-verified-install.json |
| VLMSentinelIdentityTests | a missing file fails even when the identity record exists | A missing file fails the check. | a | models/mlx_tests.rs::wrong_revision_or_missing_file_is_not_installed |

### VLMSentinelInteropTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| VLMSentinelInteropTests | the Swift writer produces the fixture's Swift record byte for byte | Swift writer matches the shared fixture. | c | Swift writer disappears; fixture stays for Rust (conformance mlx-verified-install.json) |
| VLMSentinelInteropTests | a Rust record and a Rust-ordered identity record are accepted without hashing | Swift accepts the Rust record. | c | Swift side only; Rust accepting Swift's is models/mlx_tests.rs::swift_record_is_accepted_and_gains_an_identity_record |

### QueryCommandsTests

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| QueryCommandsTests | findMergeSuggestions answers an empty library with no pairs | Empty library gives no suggestions. | a | conformance people-merge-suggestions-empty-library |
| QueryCommandsTests | findMergeSuggestions without a database emits the Rust engine's terminal error | No catalog gives a terminal error. | c | Swift mimics Rust; Rust side has server.rs::database_open_errors_name_their_cause |
| QueryCommandsTests | embedImageQuery returns the stored CLIP vector and an empty one when missing or malformed | Image query vector is the stored one. | a | conformance search-embed-image-uses-the-stored-clip-vector |
| QueryCommandsTests | embedTextQuery always answers with its query id | Text query reply keeps the id. | a | conformance search-embed-text-without-clip |
| QueryCommandsTests | generateVideoThumbnail returns a 192px JPEG and an empty-bytes reply for an unreadable video | Video thumbnails are 192px JPEG or an empty-bytes reply. | a | conformance files-video-thumbnail-undecodable-answers-empty + commands/thumbnail.rs::requested_size_defaults_and_clamps |
| QueryCommandsTests | generateVideoThumbnail honours a requested size, clamped to 64-2048, never upscaled | Requested size is clamped and not upscaled. | a | commands/thumbnail.rs::requested_size_defaults_and_clamps + larger_requests_are_honoured_but_never_upscale |

### Gaps (d), ranked

| # | Suite | Test | Cost of regression | Suggested Rust home and test shape |
|---|---|---|---|---|
| 1 | DeepAnalyzeTests | persist full pass clears requested empty outputs and stale tags | Data loss / wrong file operations or wrongly merged/lost people: A full pass replaces old description/tags even when the new output is empty. | No Rust test of the persist step clearing stale output. Add to pipeline/deep_analyze.rs or dbwriter.rs. Cost: old wrong tags/descriptions stay. CLOSED in round 17 (see its row above). |
| 2 | DeepAnalyzeTests | persist partial clears requested empty output but preserves unrequested same-model data | Data loss / wrong file operations or wrongly merged/lost people: A partial run changes only the parts asked for. | Same; add to pipeline/dbwriter.rs. CLOSED in round 17 (see its row above). |
| 3 | CatalogVectorIndexTests | rolled back embedding writes do not advance the index checkpoint | Data loss / wrong file operations or wrongly merged/lost people: A rolled-back write is not counted as indexed. | Rust tests don't roll back an embedding write and check the checkpoint. Add to catalog_index_tests.rs. CLOSED in round 17 (see its row above). |
| 4 | CatalogStoreTests | rejectsCrossFileChapterReplacementAndInvalidTiming | Data loss / wrong file operations or wrongly merged/lost people: A chapter can't replace another file's chapter and bad times are refused. | catalog.rs:218 validation has no unit test. Add to commands/catalog.rs. CLOSED in round 17 (see its row above). |
| 5 | ReassignFaceTests | newPersonIsCreatedAndEmptiedUnnamedSourceIsRemoved | Data loss / wrong file operations or wrongly merged/lost people: Reassign to a new person creates it; an emptied unnamed source is deleted. | bulk.rs handle_reassign_face new-person path and source cleanup untested. Add to commands/bulk.rs tests. |
| 6 | ReassignFaceTests | refusesMissingFaceStaleDestinationAndConflictingPayload | Data loss / wrong file operations or wrongly merged/lost people: Reassign is refused for a missing face, stale destination or contradictory payload. | Error branches of handle_reassign_face (bulk.rs ~2011-2081) untested. Add to commands/bulk.rs tests. CLOSED in round 14 (see its row above); this list entry was stale. |
| 7 | FaceClusterPersistenceTests | personLimitPreservesExistingPeople | Data loss / wrong file operations or wrongly merged/lost people: Hitting the person cap leaves existing people untouched. | MAX_PERSONS in apply()/cap() untested. Add to face_clustering_mac_tests.rs. CLOSED in round 16 as a*: the cap() function is pinned; the apply() "too many people" check is not. Round 17: the `apply()` check is pinned too; now a. |
| 8 | FaceClusteringMergeTests | preserved persons consume the hard person cap | Data loss / wrong file operations or wrongly merged/lost people: Kept people count against the 8000 person limit. | MAX_PERSONS (face_clustering_mac.rs cap()) is untested. Add to face_clustering_mac_tests.rs: preserved near cap, assert error/slots. CLOSED in round 16 as a*: the cap() function is pinned; the apply() "too many people" check is not. Round 17: the `apply()` check is pinned too; now a. |
| 9 | FaceClusteringMergeTests | mixed, legacy, stale and malformed face caches preserve identities | Data loss / wrong file operations or wrongly merged/lost people: If face vectors are mixed/legacy/stale, clustering refuses and every person and assignment is left unchanged. | CACHE_ERROR path (ensure_compatible_face_space in commands/face_clustering_mac.rs) has no test. Add to face_clustering_mac_tests.rs: two models in face_prints, run, assert error and unchanged assignments. CLOSED in round 17 (see its row above). |
| 10 | FaceClusteringMergeTests | an is_unknown person is never auto-merged | Data loss / wrong file operations or wrongly merged/lost people: Auto-merge never joins a person marked Unknown. | auto_merge (face_clustering_mac.rs:1252) has no test except the VP join; conformance covers merge SUGGESTIONS only. Add auto_merge tests to face_clustering_mac_tests.rs. CLOSED in round 14 (see its row above); this list entry was stale. |
| 11 | FaceClusteringMergeTests | a 'different' verdict pair is never auto-merged | Data loss / wrong file operations or wrongly merged/lost people: Auto-merge never joins two people the user said are different. | auto_merge untested for verdicts (pipeline consolidate_respects_blocked_pair covers the clustering stage, not auto_merge). Add to face_clustering_mac_tests.rs. CLOSED in round 14 (see its row above); this list entry was stale. |
| 12 | FaceClusteringMergeTests | Disk changes after partition selection reject a merge commit | Data loss / wrong file operations or wrongly merged/lost people: If a photo changed after planning, the merge is not committed. | require_unchanged (face_clustering_mac.rs:288) / auto_merge guard line 1417 untested beyond deferral of a changed photo before clustering. Add to face_clustering_mac_tests.rs. CLOSED in round 17 (see its row above). |
| 13 | FaceClusteringMergeTests | search overload leaves every person unchanged | Data loss / wrong file operations or wrongly merged/lost people: If the merge search exceeds its budget, no person is merged. | JOIN_EVALUATIONS/JOIN_EDGES limits untested. Add to face_clustering_mac_tests.rs. CLOSED in round 17 (see its row above). |
| 14 | FaceClusteringMergeTests | persist re-reads identity under the writer lock | Data loss / wrong file operations or wrongly merged/lost people: Names/verdicts are re-read inside the write transaction so a concurrent rename is not lost. | The in-transaction re-read (face_clustering_mac.rs:1417) has no test for a rename between plan and commit. Add to face_clustering_mac_tests.rs. Round 17: open, see its row. |
| 15 | FaceClusteringMergeTests | a bridge singleton cannot transitively merge two named persons | Data loss / wrong file operations or wrongly merged/lost people: A single face between two named people cannot chain-merge them. | auto_merge transitive guard untested (clustering-stage bridge tests exist, auto_merge ones do not). CLOSED in round 17 (see its row above). |
| 16 | FaceClusteringMergeTests | a title/middle/suffix-only-named person is treated as named | Data loss / wrong file operations or wrongly merged/lost people: A person with only a title, middle name or suffix counts as named and is protected. | Named detection in priors() (face_clustering_mac.rs:322) has no test for partial name fields. Add to face_clustering_mac_tests.rs. CLOSED in round 17 (see its row above). |
| 17 | FaceAnalysisCacheTests | refreshPreservesIdentityAndSeparatesModelEvidence | Wrong results (faces, clustering, search, Best Takes, naming): Saving a refreshed vector keeps the face's person and name and keeps each model's embeddings separate. | Rust persist test never asserts person_id or the person name survive, nor catalog_observations/catalog_embeddings rows per model. Add to commands/face_extract_mac_tests.rs: assigned face, persist model A then B, assert person_id/name unchanged and two model rows. CLOSED in round 17 (see its row above). |
| 18 | FaceAnalysisCacheTests | exclusionDoesNotReviveChangedSourceAndManualObservationSurvivesDeletion | Wrong results (faces, clustering, search, Best Takes, naming): Excluding or re-including a face does not un-stale changed-source evidence, and a hand-edited observation survives refresh/deletion. | No Rust test covers catalog_observations stale/user_edited handling during face refresh. Add to face_extract_mac_tests.rs: exclude, change source, persist; assert user_edited region_json kept and stale stays 1. CLOSED in round 17 (see its row above). |
| 19 | FaceAnalysisCacheTests | failureSnapshotCannotSuppressEditedOrHeldFaces | Wrong results (faces, clustering, search, Best Takes, naming): A recorded failure never hides a face whose source was edited or that was excluded. | pending() with an edited/excluded face and a live failure row is untested. Add to face_extract_mac_tests.rs. CLOSED in round 17 (see its row above). |
| 20 | FaceAnalysisCacheTests | malformedCurrentNamespaceVectorIsEligibleForRepair | Wrong results (faces, clustering, search, Best Takes, naming): A face whose current-model vector is malformed is treated as pending and repaired in place, keeping its person. | pending() is not tested for a present-but-malformed current vector. Add to face_extract_mac_tests.rs: store 2048-byte blob, assert pending, persist, assert person kept. CLOSED in round 17 (see its row above). |
| 21 | FaceClusteringDeterminismTests | IdentityClustering: shouldCancel aborts the pass at a safe boundary | Wrong results (faces, clustering, search, Best Takes, naming): A cancel request stops clustering at a boundary without a partial result. | Rust identity_clustering/face_clustering_mac have no cancel hook or test. Add to pipeline/identity_clustering.rs if cancel is wanted; today clustering cannot be cancelled. |
| 22 | FaceClusteringDeterminismTests | clusterShouldCancel: stale scan-cancel ignored; fresh cancel + shutdown honored | Wrong results (faces, clustering, search, Best Takes, naming): A cancel left over from an old scan does not abort a later clustering pass. | No Rust test for stale-vs-fresh cancel of a clustering run. Add to commands/face_clustering.rs (single-flight tests live there). |
| 23 | FaceClusteringMergeTests | phase-zero unknown faces do not consume the eligible window | Wrong results (faces, clustering, search, Best Takes, naming): Faces of Unknown people do not use up the per-run face limit. | load_partition limit with is_unknown persons untested. Add to face_clustering_mac_tests.rs. CLOSED in round 17 (see its row above). |
| 24 | FaceClusteringMergeTests | no-work clustering result can report preserved persons | Wrong results (faces, clustering, search, Best Takes, naming): A pass with nothing to cluster still reports the people that were kept. | Result shape for the no-work path untested (conformance people-clustering-mac... only covers a normal pass). Add to face_clustering_mac_tests.rs. CLOSED in round 17 (see its row above). |
| 25 | FaceClusteringMergeTests | oversized evidence is deferred while compatible people can merge | Wrong results (faces, clustering, search, Best Takes, naming): Too-large evidence for one person is skipped, others still merge. | No test. Add to face_clustering_mac_tests.rs. CLOSED in round 17 (see its row above). |
| 26 | FaceClusteringMergeTests | person, embedding-row, and embedding-byte preflight caps are independent | Wrong results (faces, clustering, search, Best Takes, naming): Each of the three size limits trips on its own. | MAX_PERSONS/MAX_FACES_PER_RUN/byte caps untested. |
| 27 | FaceClusteringMergeTests | high-dimensional ULP boundaries and singleton rules match the direct predicate | Wrong results (faces, clustering, search, Best Takes, naming): Pairs at the exact cosine threshold are decided identically by both join paths. | The Rust VP-join test uses generic fixtures; no exact-threshold/ULP pair. Add boundary vectors to face_clustering_mac_tests.rs. CLOSED in round 17 (see its row above). |
| 28 | FaceClusteringMergeTests | dense and adversarial searches fail before returning a partial graph | Wrong results (faces, clustering, search, Best Takes, naming): An over-dense input errors out instead of returning half a join. | JOIN_* budgets untested. CLOSED in round 17 (see its row above). |
| 29 | FaceReviewTests | migrationBackfillsExistingFacesAsBaseline | Wrong results (faces, clustering, search, Best Takes, naming): Existing faces are marked baseline so they never reappear as new. | Shared SQL migration backfill not asserted in Rust (face_review_tests.rs:131 only sets 'baseline' by hand). Add to db/migrations.rs tests. CLOSED in round 17 (see its row above). |
| 30 | TakeOutcomeAnalysisTests | parsesGoalEvidenceAndHiddenOutcomeAbstention | Wrong results (faces, clustering, search, Best Takes, naming): Model goal evidence parses; hidden outcomes make a take abstain. | Rust has take_goal.rs reply parsing for the contact-sheet ranking but no per-clip outcome-evidence parser tests (and no clip goal-evidence pipeline found). Add to commands/take_goal.rs or a new pipeline test if the feature is kept. |
| 31 | TakeOutcomeAnalysisTests | failureRequiresEveryWindowAndWholeClipSuccessRequiresCompleteEvidence | Wrong results (faces, clustering, search, Best Takes, naming): A clip fails only if every window fails; success needs complete evidence. | No Rust counterpart test. Add alongside the clip outcome code. |
| 32 | TakeOutcomeAnalysisTests | timestampMappingRejectsDuplicateOrOutOfWindowFrames | Wrong results (faces, clustering, search, Best Takes, naming): Frame timestamps outside or repeated in a window are rejected. | timeline_analysis_tests distinct_frames_drop_repeated_timestamps is for timeline moments, not take outcomes. |
| 33 | TakeOutcomeAnalysisTests | automaticScoresReachRankingWithResumeAndPreserveUserCorrections | Wrong results (faces, clustering, search, Best Takes, naming): Automatic outcome scores feed ranking, resume after pause, and keep user corrections. | No Rust test of auto outcome scores surviving user corrections. Add to commands/catalog_takes.rs. |
| 34 | TakeOutcomeAnalysisTests | goalEditsSourceChangesAndPauseRefuseLateEvidence | Wrong results (faces, clustering, search, Best Takes, naming): Late evidence after a goal edit, source change or pause is refused. | Not tested in Rust. Add to commands/catalog_takes.rs. |
| 35 | TakeOutcomeAnalysisTests | each window time takes its nearest pool frame, once, within one pool step | Wrong results (faces, clustering, search, Best Takes, naming): Window times map to distinct nearest pool frames. | video_scene_selection pool tests do not assert nearest-unique mapping. Add to pipeline/video_scene_selection.rs. |
| 36 | CatalogCastTests | detectedAndReviewedAppearancesKeepTheirEvidence | Wrong results (faces, clustering, search, Best Takes, naming): A video lists a person as Detected (face) and Reviewed (timestamped) with evidence ids. | No Rust test of person appearance hits. Add to commands/catalog_retrieval.rs. |
| 37 | CatalogCastTests | staleIgnoredOrAmbiguousIdentitiesAreAbsent | Wrong results (faces, clustering, search, Best Takes, naming): Excluded, Unknown, wrong-model, wrong-revision, changed-source or generic people never show as cast. | Same; parameterised cases excluded/unknown/revision/model/source/generic. Add to commands/catalog_retrieval.rs. |
| 38 | HNSWRecallTests | dense clusters retain routes to exact nearest neighbours | Wrong results (faces, clustering, search, Best Takes, naming): In dense clusters the graph search still returns the exact nearest neighbours. | Rust hnsw_index.rs tests (ranking_matches_cosine_similarity) use sparse data; Rust uses a different crate (instant_distance). A recall regression would hide near matches in search. Add dense-cluster recall test to util/hnsw_index.rs. CLOSED in round 17 (see its row above). |
| 39 | TakeScoringTests | goalChecksWakeTheBestInstalledVisionModel | Wrong results (faces, clustering, search, Best Takes, naming): The goal check picks the best installed vision model (not a text model). | Rust take_goal.rs only says 'unavailable' without a model; no model-choice test. Add to commands/take_goal.rs. |
| 40 | DeepAnalyzeTests | empty metadata-only results never establish model completion | Wrong results (faces, clustering, search, Best Takes, naming): A result with no model output is a failure, not 'done'. | No Rust test that an empty model result isn't recorded as complete. Add to pipeline/deep_analyze.rs. CLOSED in round 17 (see its row above). |
| 41 | DeepAnalyzeTests | video keyframe target is one quarter of duration | Wrong results (faces, clustering, search, Best Takes, naming): The video keyframe is taken at 25% of the duration. | No Rust test of the keyframe time. Add to pipeline/deep_analyze.rs. Round 17: c, the test moved to AppleKitTests (see its row). |
| 42 | DeepAnalyzeNamingTests | nameFromSoundLabel: humanizes events, drops generic labels | Wrong results (faces, clustering, search, Best Takes, naming): Sound labels become readable names. | No Rust test for sound-label naming. Add to pipeline/deep_analyze_naming.rs. |
| 43 | DeepAnalyzeNamingTests | sound analysis obeys its wall-clock deadline | Wrong results (faces, clustering, search, Best Takes, naming): Sound analysis stops after its time limit. | No Rust test. Add to the sound analysis module. |
| 44 | NearDuplicateGroupingTests | appendingSequenceKeepsTheSixtyCharacterBudget | Presentation or edge behavior: Adding a sequence suffix keeps names within 60 characters. | sequence_suffixes_match_macos does not assert the 60-char budget; conformance names-styles-budget covers the style budget only. Add to pipeline/near_duplicate_grouping.rs. CLOSED in round 17 (see its row above). |
| 45 | NearDuplicateGroupingTests | aReplyWithoutTagsLeavesTheSecondPassAvailable | Presentation or edge behavior: A reply with no TAGS line still allows the separate tag pass. | Rust test only covers the all-fields reply; add the no-TAGS case to pipeline/deep_analyze_naming.rs. CLOSED in round 17 (see its row above). |
| 46 | NearDuplicateGroupingTests | systemPromptAsksForTagsOnlyWhenMerged | Presentation or edge behavior: The prompt requests TAGS only in merged mode. | models/vlm.rs prompt builders only have rename_prompt_forbids_invented_dates. Add a prompt test in models/vlm.rs. CLOSED in round 17 (see its row above). |
| 47 | CatalogTextSearchTests | evidencePromptIsNumberedAndBounded | Presentation or edge behavior: The chat evidence prompt numbers hits and is size-bounded. | No Rust test of the evidence prompt. Add to commands/chat.rs. CLOSED in round 17 (see its row above). |
| 48 | CatalogVectorIndexTests | matching moments cannot crowd other files out of a Library result | Presentation or edge behavior: Many matches in one video don't push out other files. | No Rust test limits per-file moments in a Library result. Add to catalog_index_tests.rs or commands/catalog_search.rs. |
| 49 | CatalogVectorIndexTests | concurrent refreshed queries retain complete graph ownership | Presentation or edge behavior: Queries during an index refresh see a whole graph. | catalog_index uses Arc<HnswMap>; no concurrent-refresh test. Add to catalog_index_tests.rs. |


### Round 16: area C

The only "not individually verified" row of this area (FaceAlignTests, 90 degree rotation) was read and confirmed: the shared fixture case asserts the fit. The partial rows named in their own text (face failure back-off window, running-sum Pass-2, hybrid search without a vector, the deep-analyze completion flag) already say what is not asserted and were not changed. Closed: the person cap function (`commands/face_clustering_mac_tests.rs`, two tests, failed when the slots, the take count and the protected keep were each broken). Gap-list entries 6, 10 and 11 were already closed in round 14 and are marked so. The rest of the ranked list below is open.

## Area D: chat, timeline, video and media tools


Scope: 18 suites under `platforms/apple/Tests/EngineTests` (files with ' 2' in the name ignored). Counts are `grep -cE '@Test|func test'` per file; a parameterised `@Test(arguments:)` counts once.

Legend. **a** = pinned by a Rust unit test whose assertion fails if the invariant breaks. **a\*** = pinned except for one named sub-invariant, which is listed in Gaps. **b** = shared conformance case. **c** = Swift-only (disappears with the engine) or shared Swift code that stays. **d** = gap.
Paths: `E` = `platforms/windows/src/engine/src`. `AK` = `platforms/apple/Tests/AppleKitTests/MediaAdapterTests.swift` (Swift tests of the adapters in `platforms/apple/adapters`, which stay). Conformance cases are in `shared/test-corpus/conformance/*.json`. Shared JSON fixtures: `shared/test-corpus/chat-intents.json`, `chat-selection.json`, `chat-search.json`.

Context that changes several rows: reframing, stabilization, the frame pool and the common recipes were moved to `FileIDAppleKit` (`MediaReframing.swift`, `MediaStabilization.swift`, `MediaExport.swift`, `MediaFrames.swift`). That code stays, but its only unit tests are in the EngineTests suites being deleted. `AK` covers it end to end (codec, aspect ratio, summary prefix) and little else. The `cross-*` video and media conformance cases run `engines:["rust"]` with `requires:["swift","rust"]`, so they stop running when the Swift engine is deleted (SWIFT_ENGINE_RETIREMENT.md, `conformance.py` row).

### ChatActionTests (10)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ChatActionTests | sharedIntentCases | Every shared intent fixture parses to the same intent JSON. | a | E/commands/chat_intent.rs::shared_intent_cases (same chat-intents.json) |
| ChatActionTests | sharedQuestionCases | Every shared question fixture parses to the same question JSON. | a | E/commands/chat_intent.rs::shared_question_cases |
| ChatActionTests | sharedLabels | Byte and count labels and plural nouns format identically. | a | E/commands/chat_intent.rs::shared_labels |
| ChatActionTests | mergeCountsOnlyFilesWithActiveFaces | A people-merge proposal counts only files with a non-excluded face of the merged people. | a | E/commands/chat_actions.rs::merge_counts_only_files_with_active_faces |
| ChatActionTests | mutatingKindsAlwaysRequireConfirmation | Only the six mutating kinds need confirmation, and invalid trash, page and rename proposals are dropped (no self-keeper, no `../`). | a | E/commands/chat_actions.rs::mutating_kinds_always_require_confirmation |
| ChatActionTests | modelProposalsAreValidatedAndGatedByTheUsersWords | Model-proposed actions are parsed strictly and run only if the user's own words authorise them. | a | E/commands/chat_actions.rs::model_proposals_are_validated_and_gated_by_the_users_words |
| ChatActionTests | documentInstructionsNeverBecomeActions | Instructions inside a document are data: no action, no file change. | a+b | E/commands/chat_actions.rs::document_instructions_never_become_actions; chat-ignores-instructions-inside-files |
| ChatActionTests | duplicateTrashProposalListsExactCopiesAndKeepers | "Move extra copies to the trash" proposes the exact copies, their keepers and byte count, needs confirmation and changes nothing. | a+b | E/commands/chat_actions.rs::duplicate_trash_proposal_lists_exact_copies_and_keepers; chat-proposes-actions-for-requests |
| ChatActionTests | structuredQuestionsAnswerFromTheCatalog | Count, size and duplicate questions are answered from SQL with exact wording and cited files. | a+b | E/commands/chat_facts.rs::counts_sizes_dates_and_duplicates; chat-structured-answers-from-the-catalog |
| ChatActionTests | fusionIsStableAndWeighted | Weighted rank fusion of search groups orders hits stably and keeps the chapter evidence id. | a (closed round 16; was d) | platforms/windows/src/engine/src/commands/chat.rs::tests::rank_fusion_rewards_agreement_keeps_the_evidence_id_and_breaks_ties_by_first_appearance |

### ChatComplexQueryTests (1)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ChatComplexQueryTests | bench (complex questions get the right files and numbers) | A subject matches every file it describes (name, description, tags, text), so counts, sizes, orders, "and/or" people and exclusions are exact. | a+b | E/commands/chat.rs::complex_questions_see_every_matching_file; chat-complex-questions-see-every-match (32 steps) |

### ChatHistoryPromptTests (6; includes ChatModelWakeTests in the same file)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ChatHistoryPrompt | keepsOnlyTheLastTurnsAndTrimsThem | The model prompt carries only the last four turns, each trimmed. | a | E/commands/chat.rs::history_keeps_the_last_turns_trimmed |
| ChatHistoryPrompt | emptyHistoryAddsNothing | Empty history adds nothing and the prompt starts with "Request:". | a | chat.rs::history_keeps_the_last_turns_trimmed (empty), evidence_prompt_is_numbered_and_bounded (prefix) |
| ChatHistoryPrompt | evidenceNamesTheCaptureDateAndRecognisedPeople | Evidence lines name the capture date and confirmed, non-excluded, named people. | a | E/commands/chat.rs::evidence_names_the_capture_date_and_recognised_people |
| ChatHistoryPrompt | historyPrecedesTheRequest | The history block comes before the "Request:" line in the prompt. | a (closed round 16; was d) | platforms/windows/src/engine/src/commands/chat.rs::tests::earlier_turns_come_before_the_request_in_the_prompt (the join was extracted as `with_history`) |
| ChatModelWake | onlyAKnownInstalledModelIsEverWoken | A chat summary never loads a model id that is not a known installed kind. | a (closed round 16; was d) | platforms/windows/src/engine/src/commands/chat.rs::tests::a_summary_never_wakes_a_model_kind_that_is_not_a_known_installed_one |
| ChatModelWake | anIdleEngineMayWakeAModel | An idle engine is allowed to wake a model. | c | Swift-only `ChatService.canWake`; Rust uses `resident_service` waiters |

### ChatLibraryAnswersTests (9)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ChatLibraryAnswers | overviewNamesKindsDatesAndBusiestFolders | "What is in my library" answers totals, kinds, date range and busiest folders from the catalog. | a | E/commands/chat_facts.rs::library_overview_and_folder_questions_read_totals_not_words |
| ChatLibraryAnswers | folderQuestionsRankByCountOrSizeAndFoldWindowsSeparators | Folder questions rank by count or size and treat `\` and `/` alike. | a | same test (ranking) plus chat_facts.rs::folder_filters_match_windows_separators |
| ChatLibraryAnswers | foldersWithTheSameNameAreToldApartByTheirParent | Two folders named Downloads are shown with their parent. | a | chat_facts.rs::folders_with_the_same_name_are_told_apart_by_their_parent |
| ChatLibraryAnswers | foldersThatStillCollideGetMoreOfTheirPath | Folders that still collide get more of their path. | a | chat_facts.rs::folders_that_still_collide_get_more_of_their_path |
| ChatLibraryAnswers | folderTotalsWalkTheDirectoryIndex | Folder totals group through the `catalog_files_dir` index, not a temp B-tree. | a | chat_facts.rs::folder_totals_walk_the_directory_index |
| ChatLibraryAnswers | formatWordsFilterByExtension | "RAW", "jpgs", "pngs" filter by extension with exact wording. | a | chat_facts.rs::format_words_filter_by_extension |
| ChatLibraryAnswers | aNamedLibraryGetsTheOverviewButALibraryOfThingsIsASearch | "My Adlon library" is an overview and "library of dogs" is a search. | a | chat_facts.rs::a_named_library_gets_the_overview_but_a_library_of_things_is_a_search |
| ChatLibraryAnswers | perTypeQuestionsGetTheLibraryBreakdown | "Break down by type" gets the breakdown and "photos by Kyle" does not. | a | chat_facts.rs::per_type_questions_get_the_library_breakdown |
| ChatLibraryAnswers | ordinaryQuestionsAreNotCapturedAsLibraryQuestions | "How many files in my library" is a count and "biggest file" is a list. | a | chat_facts.rs::library_overview_and_folder_questions_read_totals_not_words (last four assertions) |

### ChatSearchPlanTests (9)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ChatSearchPlan | confirmedPersonNamesBecomeIdentityFilters | Only confirmed person names become identity filters; an ambiguous first name matches every candidate; refinements keep them. | a | E/commands/chat_search.rs::confirmed_names_events_time_refinement_and_negation, both_people_excluded_people_and_follow_ups |
| ChatSearchPlan | pronounsNarrowTheLastSearch | "Which of them show Alex" and "what is he wearing" narrow the previous search. | a | chat_search.rs::pronouns_narrow_the_last_search |
| ChatSearchPlan | sharedPlansPreserveSubjectsAndExplicitFilters | Shared multi-message plans keep subjects and explicit kinds. | a | chat_search.rs::shared_plans_preserve_subjects_and_explicit_filters (chat-search.json) |
| ChatSearchPlan | chaptersRankFirst | Named chapters outrank verified passages, which outrank unverified sampled frames. | a | E/commands/catalog_retrieval.rs::chapters_rank_first |
| ChatSearchPlan | filtersApplyBeforeLimitsAndToEvidence | Kind filters apply before the 100-hit limit and to evidence; "only videos" refines the last query. | a | catalog_retrieval.rs::filters_apply_before_limits_and_exclude_failed_stale_and_unknown; chat.rs::refinement_filters_files_and_evidence_before_limits |
| ChatSearchPlan | personFilterRequiresAConfirmedObservation | A person filter returns only files with a person observation, labelled unverified or user-confirmed, with the time. | a* | catalog_retrieval.rs (user-confirmed branch only); the "unverified appearance" label of a non-edited observation is not asserted |
| ChatSearchPlan | eventAndTimeFiltersSurviveConversationalRefinement | Named event and timestamp survive "only photos". | a | chat_search.rs::confirmed_names_events_time_refinement_and_negation |
| ChatSearchPlan | invalidTimestampRemainsSearchText | "2:99" is not a time; "01:02:03" is 3723 s. | a | chat_search.rs (timestamp("1:60"), "1:2:60" rejected; 01:02:03 = 3723) |
| ChatSearchPlan | namedEventAndTimestampReturnMatchingChapterEvidence | A named event plus a timestamp returns only that event's chapter at that time, and the reply names both. | a* | catalog_retrieval.rs::filters_apply_before_limits_and_exclude_failed_stale_and_unknown (event + time window); the reply wording ("Grandma Birthday", "2:30") is not asserted |

### ChatSelectionTests (10)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ChatSelection | sharedCriteriaCases | Criteria extraction matches the shared fixtures. | a | E/commands/chat_selection.rs::shared_criteria_cases (chat-selection.json) |
| ChatSelection | sharedChecksCases | Per-criterion check prompts match the shared fixtures. | a | chat_selection.rs::shared_checks_cases |
| ChatSelection | sharedCombineCases | Combining verdicts matches the shared fixtures. | a | chat_selection.rs::shared_combine_cases |
| ChatSelection | confirmationsAskAboutTheWholeRequestThenEachPart | The model is asked about the whole request, then each part; small models get one prompt. | a | chat_selection.rs::confirmations_ask_about_the_whole_request_then_each_part |
| ChatSelection | checkModelPrefersAnInstalledSmallModelThenTheBestThatFits | The check model is the installed small Qwen, else the largest that fits memory, else none. | a | chat_selection.rs::check_model_prefers_an_installed_small_qwen_then_the_largest_that_fits |
| ChatSelection | sharedVerdictCases | Model verdict replies parse to yes, no or unsure with a reason. | a | chat_selection.rs::shared_verdict_cases |
| ChatSelection | sharedRankingCases | CLIP, tag, description and people evidence rank the attached files with exact scores and reasons. | a | chat_selection.rs::shared_ranking_cases |
| ChatSelection | sharedMessageCases | Checked, evidence, progress and stopped messages are worded identically. | a | chat_selection.rs::shared_message_cases |
| ChatSelection | unsureAndNoNeverCountAsMatches | Only yes and unsure appear in results, yes first; no is dropped. | a | chat_selection.rs::unsure_and_no_never_count_as_matches |
| ChatSelection | scopeNeverReadsOutsideTheSelection | A scoped send reads only the attached, non-failed media files, validates the id list, and returns nothing outside it. | a+b | chat_selection.rs::scope_never_reads_outside_the_selection; chat-selection-answers-only-from-the-attached-files |

### ChatServiceTests (3)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ChatServiceTests | searchHistoryAndClearRemainLocalAndPersistent | Send stores two turns, history returns them, clear removes them, and the catalog files are untouched. | a | E/commands/chat.rs::history_search_and_clear_preserve_catalog_files |
| ChatServiceTests | emptyAndOversizedRequestsCannotWriteHistory | Empty, whitespace-only and over-2000-character texts are refused and write no history. | a (closed round 17; was a*) | platforms/windows/src/engine/src/commands/chat.rs::tests::invalid_requests_cannot_write_history_or_change_files (now with " " and "\n\t "; failed with the trim removed) |
| ChatServiceTests | requestsRoundTripWithoutNullFields | A chat request encodes without null fields, round-trips, and "Please show me my birthday videos" searches for "birthday". | c | Swift `IPCCoder` and DTO encoding in FileIDShared: stays, moves to SharedTests. The query half is `chat.rs::invalid_requests_cannot_write_history_or_change_files` (a) |

### ChatYieldTests (3)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| ChatYield | parkedSummariesRunOnceInOrderBetweenFiles | Parked chat summaries run once, in order, between Describe files. | c | Swift-only `ChatYield`. Rust yields through `resident_service::has_waiters` (deep_analyze_mlx.rs:293; resident_owner_tests.rs). Product-level interleave is a Gap |
| ChatYield | theQueueFallbackSkipsASummaryTheBatchAlreadyRan | A summary the batch already ran is not run again by the queue. | c | Swift-only mechanism |
| ChatYield | aSummaryTheQueueReachesFirstIsNotRunAgainByTheBatch | A summary the queue ran first is not run again by the batch. | c | Swift-only mechanism |

### TimelineChapterSuggestionsTests (4)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TimelineChapterSuggestions | proposesOnlyAConfirmedCaptionTransitionAndMarksItAsADraft | A chapter is proposed only at a confirmed caption change, as a low-confidence unedited draft with the suggestion version prefix. | a* | E/pipeline/timeline_analysis_tests.rs::chapter_suggestions_split_at_confirmed_transitions (splits, end 19.999, version prefix); confidence 0.2, not user-edited and not stale are not asserted for suggestions |
| TimelineChapterSuggestions | abstainsWhenSamplesDoNotConfirmAChange | Captions about the same scene produce no chapter. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/timeline_analysis_tests.rs::similar_captions_confirm_no_chapter_change |
| TimelineChapterSuggestions | abstainsWhenThereAreTooFewSamples | One caption produces no chapter. | a | timeline_analysis_tests.rs::chapter_suggestions_split_at_confirmed_transitions (`captions[..2]` empty) |
| TimelineChapterSuggestions | persistsSuggestionsAndKeepsAcceptedEditsAcrossRegeneration | Regenerating suggestions replaces drafts and keeps user-edited chapters. | a | E/commands/timeline_tests.rs::chapter_suggestions_replace_drafts_but_keep_user_edits (+ chapter_suggestions_persist_when_source_is_unchanged) |

### TimelineMomentAnalysisTests (13)
The pure functions are in `shared/Sources/FileIDVLMCore/TimelineMomentAnalysis.swift`, used only by the engine, so they go dead with it. Rust owns the algorithm now.

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TimelineMomentAnalysis | quietFootageRetainsOverlappingFallbackWindows | Footage without motion still gets overlapping eight-frame windows. | a | timeline_analysis_tests.rs::moment_windows_overlap_and_place_peak, short_video_gets_single_window |
| TimelineMomentAnalysis | motionPeakDoesNotRemoveQuietCoverage | A motion peak joins its window's frames without dropping other windows; non-finite signals are ignored. | a | timeline_analysis_tests.rs::moment_windows_overlap_and_place_peak (non-finite signals not fed) |
| TimelineMomentAnalysis | invalidOrUnboundedDurationsAreRejected | Zero, negative, NaN, infinite and over-6-hour durations yield no windows. | a | timeline_analysis_tests.rs::moment_windows_overlap_and_place_peak (0 and MAX+1) |
| TimelineMomentAnalysis | parsesEvidenceAndAbstentionWithoutInventingConfidence | Model JSON becomes unedited draft chapters at confidence 0.2; an empty list is a valid abstention. | a | timeline_analysis_tests.rs::parse_moments_accepts_wrapped_json_and_rejects_bad_ranges, moment_chapters_map_frames_to_intervals |
| TimelineMomentAnalysis | rejectsUnboundedOrMalformedModelEvidence | Prose, out-of-range, reversed, blank-title and over-16 KB replies are rejected. | a (closed round 17; was a*) | platforms/windows/src/engine/src/pipeline/timeline_analysis_tests.rs::a_moment_reply_over_sixteen_kilobytes_is_refused (16,384 bytes parse, 16,385 refused; failed with the cap moved by one) and parse_moments_accepts_wrapped_json_and_rejects_bad_ranges |
| TimelineMomentAnalysis | promptShowsTypesNotCopyableSampleValues | The prompt lists frame times, shows types not sample values, and marks the frames untrusted. | a | timeline_analysis_tests.rs::prompt_lists_every_frame_time, prompt_shows_types_not_copyable_sample_values |
| TimelineMomentAnalysis | labelledRequestInterleavesTimestampsWithFrames | The MLX request interleaves "[t=..s]" labels with images and asks for start/end seconds. | c | Swift-only (Qwen3VLMessageGenerator, CIImage); the Rust prompt has no labelled form |
| TimelineMomentAnalysis | unlabelledRequestKeepsTheFrameIndexPrompt | Without labels the request is the frame-index prompt with no image labels. | c | Swift-only MLX request shape |
| TimelineMomentAnalysis | parsesSecondsAndSnapsThemToFrames | Quoted seconds ("t=10.45s") snap to the nearest frame and set chapter bounds. | a | timeline_analysis_tests.rs::parse_moments_snaps_quoted_seconds_to_frames |
| TimelineMomentAnalysis | fallsBackToFrameIndicesOrTheWholeSequence | Replies without usable times fall back to frame indices or the whole window; impossible ranges are rejected. | a | timeline_analysis_tests.rs::parse_moments_falls_back_to_frame_indices_or_the_whole_sequence |
| TimelineMomentAnalysis | ignoresInvalidDecodedTimestamps | A chapter is built only from finite, increasing frame times. | a (closed round 16; was d) | platforms/windows/src/engine/src/pipeline/timeline_analysis_tests.rs::moment_chapters_need_finite_increasing_frame_times_inside_the_window |
| TimelineMomentAnalysis | deduplicatesLowFrameRateSamplesWithoutInventingExtraEvidence | Repeated decoded timestamps are dropped and chapters map to the distinct frames. | a | timeline_analysis_tests.rs::distinct_frames_drop_repeated_timestamps, moment_chapters_map_frames_to_intervals |
| TimelineMomentAnalysis | transactionalWindowPersistenceKeepsUserEditsAndSampledCoverage | A window write keeps user-edited chapters, replaces drafts, records coverage, and is refused for a paused job or a changed source. | a* | timeline_tests.rs::worker_moment_windows_keep_user_edits_and_refuse_changed_sources; "job no longer running refuses the write" is not tested |

### TimelineSignalAnalysisTests (5)
Code is `shared/Sources/FileIDVLMCore/TimelineSignalAnalysis.swift`. The retirement doc 5.2 lists the suite as staying.

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TimelineSignalAnalysis | preservesUniformCoverageWhenNoStrongChangeExists | With no signals, sampling is one frame per interval. | c | stays, moves to SharedTests. Same algorithm: timeline_analysis_tests.rs::sample_times_prefer_peaks_and_fall_back_to_boundaries |
| TimelineSignalAnalysis | selectsTheStrongestVisualChangeInsideEachSamplingWindow | The strongest change in each window replaces its boundary sample. | c | stays, moves to SharedTests. Rust: sample_times_prefer_peaks_and_fall_back_to_boundaries |
| TimelineSignalAnalysis | fallsBackToWindowBoundaryForWeakChangesAndRejectsInvalidSignals | Weak or invalid signals fall back to the window boundary. | c | stays, moves to SharedTests. Rust has the weak case only; NaN and over-range signals are not fed |
| TimelineSignalAnalysis | keepsBaselineAndRejectsNearDuplicateCandidate | A candidate too close to the baseline frame is rejected. | c | stays, moves to SharedTests. Rust: sample_times_respect_spacing_and_reject_invalid_input |
| TimelineSignalAnalysis | invalidDurationAndIntervalProduceNoSamples | Infinite duration or zero interval yields no samples. | c | stays, moves to SharedTests. Rust: sample_times_respect_spacing_and_reject_invalid_input |

### TimelineSpeechTranscriptionTests (6)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| TimelineSpeechTranscription | splitsLongRecordingsIntoBoundedChunksWithOverlap | Long audio is split into owned spans with overlapping extracts (1.5 s), with no gaps. | a | E/pipeline/speech_passages.rs::chunks_own_consecutive_spans_and_overlap_their_extracts |
| TimelineSpeechTranscription | groupsTimedWordsIntoSearchablePassages | Timed words group into passages with joined punctuation, start, end and mean confidence. | a | speech_passages.rs::passages_break_on_pauses_span_and_length_and_join_punctuation; timeline_tests.rs::speech_pass_transcribes_audio_chunks_and_resumes_by_chunk (stored time and confidence) |
| TimelineSpeechTranscription | overlapOnlyKeepsSpeechInItsOwningChunkAndClampsAtVideoEnd | A word belongs to the chunk that owns its midpoint, and ends clamp to the media duration. | a (closed round 17; was a*) | platforms/windows/src/engine/src/pipeline/speech_passages.rs::tests::non_finite_words_are_dropped_and_a_passage_never_ends_after_the_video (failed with both end clamps removed) and words_belong_to_the_chunk_that_owns_their_midpoint |
| TimelineSpeechTranscription | rejectsInvalidChunkAndInvalidTimingInputs | Non-finite durations, blank words, NaN timestamps and infinite confidence produce nothing. | a (closed round 17; was a*) | platforms/windows/src/engine/src/pipeline/speech_passages.rs::tests::non_finite_words_are_dropped_and_a_passage_never_ends_after_the_video (NaN timestamp, infinite duration, infinite and NaN confidence, NaN and infinite media duration; failed with the finite check removed) |
| TimelineSpeechTranscription | sweepRemovesOnlyOldUuidNamedAudioTemps | Old `fileid-speech-<uuid>.m4a` temps are swept; recent and unrelated files are kept. | c | Swift-only sweep; the adapter (MediaSpeech.swift:70-72) removes its temp with `defer` and has no sweep |
| TimelineSpeechTranscription | persistsTimestampedPassagesAndInvalidatesChangedSources | Passages persist with time, text, confidence, FTS row and verified coverage, and become stale when the source file changes. | a (closed round 17; was a*) | platforms/windows/src/engine/src/commands/catalog.rs::tests::passages_observations_and_coverage_go_stale_when_the_source_changes (the `catalog_invalidate` trigger for passages, coverage and observations; a stale passage never answers a search; failed with the passage line removed from the trigger) and timeline_tests.rs::speech_pass_transcribes_audio_chunks_and_resumes_by_chunk |

### VideoConversionTests (5)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| VideoConversion | videoAndAudioTimingSurviveNativeConversion | A video export keeps AAC audio and duration within 0.1 s, strips location metadata, keeps the source bytes. | c (round 17; was d) | Mac adapter code (FileIDAppleKit `MediaExport.video`) that stays; ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaVideoExportTests.videoAndAudioTimingSurviveConversionAndLocationIsStripped (location: outcome only; AVAssetExportSession dropped it in round 17 even with the metadata filter removed) |
| VideoConversion | portraitExportReopensPreservesOriginalAndSupportsUndo (fit, auto) | A 16:9 export of a portrait clip is named "16x9 Fit" or "16x9 People", the notes show full-picture samples, the output is registered as a video, and undo removes it with the source intact. | b | video-tools-preview-notes-app-hosted (names and notes); cross-video-swift-preview-rust-execute-fit (execute, undo, source sha). The cross case needs the Swift engine. Cargo test `tools.rs::video_preview_execute_and_undo_keep_original_and_recovery` runs only with FILEID_FFMPEG_DIR |
| VideoConversion | framedExportsPreservePictureTimingAndAudio | Every frame size (9:16, 16:9, 1:1, 4:5 at 1280, 9:16 at 1920) hits its ratio and size bound, keeps AAC audio and duration, and pads rather than stretches. | c (round 17; was d) | Mac adapter code that stays; ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaVideoExportTests.framedExportsPreservePictureTimingAndAudioAndPadRatherThanStretch |
| VideoConversion | workerRejectsProtectedOutputBeforeSourceAccessAndRejectsBrokenInput | A protected output path, a broken input and an unsupported size are refused. | a | tools.rs::undo_keeps_edited_exports_and_rejects_protected_destinations (protected path); AK::probeReadsCodecsTimingAndRotation (unreadable input); AK::videoExportFitsPadsAndStabilizes (bad size) |
| VideoConversion | conventionalVideoEnlargementIsOptInAndPreservesTiming | Opt-in video enlargement exceeds the source size, keeps duration and audio, and leaves the source alone. | c (round 17; was d) | Mac adapter code that stays; ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaVideoExportTests.conventionalVideoEnlargementIsOptInAndPreservesTiming |

### VideoFrameWorkerTests (9)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| VideoFrameWorker | sequenceRejectsUnboundedAndDuplicateDestinationsBeforeReadingSource | The Swift frame-sequence worker refuses oversized, empty, unordered or duplicate-destination requests. | c | Swift-only `runSequence`. Rust uses `--sample-video` and `--sample-video-pool` (apple/media.rs) |
| VideoFrameWorker | sweepsOnlyOldWorkerFrames | Only old worker frame files are swept; active and unrelated files stay. | c | Swift-only sweep. Rust removes its frames root in `timeline::recover` and asserts temp frames are gone (timeline_tests.rs::a_malformed_moment_reply_leaves_only_its_window_incomplete) |
| VideoFrameWorker | sweepsTheFrameFolderOncePerGate | The frame folder is swept once per gate. | c | Swift-only |
| VideoFrameWorker | sweepGateIsClaimedOnceAcrossThreads | A sweep gate is claimed by exactly one of 64 racing threads. | c | Swift-only |
| VideoFrameWorker | timeoutTerminatesWorker | A worker that exceeds its timeout is killed and its wait ends. | d | `native_process::self_worker::run` (the Mac worker launcher, native_process.rs:53-84) has no test. video_export.rs tests cover only the FFmpeg `wait` Round 17: still open. `self_worker` exists only in the `apple-adapters` build and reads the worker path from FILEID_TEST_ENGINE_PATH, which the Mac media tests also use, so a stand-in worker would race them; `native_process.rs` is digest-locked, so no seam was added. |
| VideoFrameWorker | workerRefusesProtectedOutputsBeforeReadingSource | The worker refuses a protected output before opening the source. | c | Swift-only worker. Rust writes frames only into its own temp directory |
| VideoFrameWorker | cancellationWaitsForWorkerExit | Cancelling stops the worker and returns only after it has exited. | d | Same as timeoutTerminatesWorker: `self_worker::run` cancel path is untested Round 17: still open, as timeoutTerminatesWorker. |
| VideoFrameWorker | sourceRevisionToleratesTheOtherEnginesRoundingOfTheSameModifiedTime | A source revision written with either engine's rounding of the same mtime matches. | a | E/commands/timeline_tests.rs::on_disk_check_accepts_either_engines_rounding_of_the_same_modified_time |
| VideoFrameWorker | sourceRevisionStillRejectsARealChange | A different size or mtime (+1 ms) is a changed source. | a | same test (`moved` rejected); "unknown" revision handling is not asserted |

### VideoReframingTests (4)
Logic is `platforms/apple/adapters/Sources/FileIDAppleKit/MediaReframing.swift` (stays). Tests are only in EngineTests.

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| VideoReframing | smoothCropsKeepAllObservedPeopleAndFitOnLostOrUnsafeGroups | Follow-people crops contain every observed person, keep the ratio, move smoothly, and fall back to the full picture when people are lost or the group will not fit. | c (round 17; was d) | `MediaReframing.plan` (FileIDAppleKit) stays; ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaReframingPlanTests.smoothCropsKeepAllObservedPeopleAndFitOnLostOrUnsafeGroups |
| VideoReframing | invalidEvidenceCannotProduceACropAndTimestampsAreChecked | Out-of-frame, zero-size or NaN boxes, decreasing times and long gaps never produce a crop. | c (round 17; was d) | Ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaReframingPlanTests.invalidEvidenceCannotProduceACropAndTimestampsAreChecked |
| VideoReframing | generatedMovingSubjectsFollowCropRampsAndPreferredOrientation | A moving subject stays centred in the exported video, in both landscape and rotated sources. | c (round 17; was d) | Ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaReframingPlanTests.generatedMovingSubjectsFollowCropRampsAndPreferredOrientation (both orientations) |
| VideoReframing | autoWorkerWithoutPeopleFitsAndReportsFallback | Auto framing with no people fits the picture, reports "0 followed samples ... full-picture samples", and refuses over-long videos. | b + c (round 17) | video-tools-preview-notes-app-hosted ("people" step). The fallback summary and the 601 s refusal are in platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaReframingPlanTests.autoFramingWithoutPeopleFitsReportsFallbackAndRefusesLongVideos |

### VideoStabilizationTests (4)
Logic is `.../FileIDAppleKit/MediaStabilization.swift` (stays).

| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| VideoStabilization | nativeRegistrationEstimatesRealTranslationAndRotation | Frame alignment recovers known translation and rotation to within 2.5 px and 0.01 rad. | c (round 17; was d) | `MediaStabilization.alignment` (FileIDAppleKit) stays; ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaStabilizationPlanTests.nativeRegistrationEstimatesRealTranslationAndRotation |
| VideoStabilization | smoothingCorrectsKnownJitterAndNeverPublishesExposedBorders | Smoothing corrects known jitter, caps zoom at 1.12, never exposes a border, and leaves unsafe or unregistered footage uncorrected. | c (round 17; was d) | Ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaStabilizationPlanTests.smoothingCorrectsKnownJitterAndNeverPublishesExposedBorders |
| VideoStabilization | actualWorkerReducesGeneratedTranslationAndRotationJitter | A stabilized export has at least 35% less translation jitter and 25% less rotation jitter, with duration and aspect kept. | c (round 17; was d) | Ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaStabilizationPlanTests.stabilizedExportReducesGeneratedTranslationAndRotationJitter (through `MediaExport.video`) |
| VideoStabilization | stabilizedSocialExportPersistsRecipeRelationshipsAndUndo | A stabilized 9:16 people export is named "9x16 People - Stabilized", stores its recipe, comes out 9:16 and undoes cleanly. | b + a (round 17) | video-tools-preview-notes-app-hosted ("stabilized"); cross-video-rust-preview-swift-execute-stabilized (needs Swift). Stored recipe: platforms/windows/src/engine/src/commands/tools.rs::tests::an_export_stores_the_recipe_it_ran_beside_its_lineage (the same `recipe_json` write serves every kind; shown on a photo export; failed with the stored recipe replaced by "{}") |

### MediaToolsTests (7)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| MediaTools | enlargementIsOptInAndPreservesOrientationOriginalAndUndo | Photo enlargement happens only when asked, keeps the source bytes, and undo is recoverable and recorded in history. | a | E/commands/tools.rs::enlargement_is_opt_in_preserves_sources_and_undo |
| MediaTools | enlargementKeepsAspectBoundsAndRejectsOtherTools | Enlarged sizes keep aspect within the bound, and `supports()` refuses 8193 px, chapters+upscale, photo+framing/aspect/stabilization, ratio 10:3, unknown framing and auto framing without a ratio. | a* (closed round 16; was d) | platforms/windows/src/engine/src/commands/tools.rs::tests::a_photo_export_refuses_video_only_options_and_every_video_choice_it_cannot_honour (photo + framing/aspect/stabilization, 8193, chapters + upscale; the video refusals (10:3, unknown framing) also hold when the runtime is absent, so they are not pinned apart from it) |
| MediaTools | conversionAppliesOrientationStripsMetadataAndSurvivesUndoReopen | A photo export applies EXIF orientation, strips GPS and EXIF comment, registers a derivative and survives undo after reopen. | a (closed round 17; was d) | platforms/windows/src/engine/src/commands/tools.rs::tests::a_photo_export_applies_the_exif_orientation_and_keeps_no_location (a JPEG with EXIF orientation 6 and a GPS block; JPEG and PNG outputs are upright and carry no EXIF; failed with `apply_orientation` removed). Undo and reopen: conversion_validates_outputs_links_derivatives_and_undo_is_recoverable |
| MediaTools | changedSourceAndOccupiedOutputAreRejectedWithoutOverwriting | Execute refuses an occupied output (bytes kept) and a changed source (nothing written). | a | tools.rs::execution_rejects_changed_sources_and_occupied_outputs |
| MediaTools | chapterExportsAreEvidenceSnapshotsAndProtectedPathsFailBeforeWrite | VTT cues are escaped with non-zero intervals, a protected destination is refused, and editing a chapter after preview refuses the execute. | a (closed round 17; was a*) | platforms/windows/src/engine/src/commands/tools.rs::tests::a_chapter_export_refuses_markers_edited_added_or_deleted_after_its_preview (failed with the marker comparison at execute removed), chapter_export_escapes_cues_and_uses_nonzero_intervals, undo_keeps_edited_exports_and_rejects_protected_destinations |
| MediaTools | undoProtectsEditedExportsAndInterruptedOperationsRemainInspectable | Undo refuses an edited export and keeps it; an interrupted operation is marked failed. | a (closed round 14; was a*) | platforms/windows/src/engine/src/commands/tools.rs::tests::restart_cleanup_never_deletes_what_a_stage_path_links_to (interrupted export marked failed). Was: tools.rs::undo_keeps_edited_exports_and_rejects_protected_destinations; `recover` setting state to failed is not asserted |
| MediaTools | recoveryCleansOnlyRegisteredStagingAndPreservesSourcesAndAliases | Restart cleanup removes only registered `.FileIDExport-<uuid>.part` files and never touches sources, unrelated files or a symlink alias to the source. | a (closed round 14; was a*) | platforms/windows/src/engine/src/commands/tools.rs::tests::restart_cleanup_never_deletes_what_a_stage_path_links_to. Was: tools.rs::restart_cleanup_preserves_unrelated_files_and_sources (stage removed, unrelated and source kept); the symlink alias case is not planted |

### CommonMediaToolsTests (9)
| Suite | Test | Invariant | Cat | Rust pin |
|---|---|---|---|---|
| CommonMediaTools | recipeValidationRefusesWithSharedMessages | Every invalid trim, remux, audio, proxy and rotate recipe is refused with the shared message; valid ones pass. | a+b | E/commands/media_recipes.rs::validation_refuses_with_messages_shared_with_the_mac_engine; media-tools-recipe-validation-and-refusals |
| CommonMediaTools | verificationRejectsChangedStreamsTimingSyncAndOrientation | An output with changed codec, duration, audio sync, lost audio or wrong orientation is rejected. | a | media_recipes.rs::verification_rejects_changed_streams_timing_sync_and_orientation |
| CommonMediaTools | remuxCopiesStreamsStripsLocationAndUndoRestores | Remux copies streams, keeps rotation and duration, strips location metadata, links the derivative as "conversion" and undoes. | a + c (round 17; was a*) | media_recipes.rs::arguments_copy_losslessly_and_re_encode_only_when_asked (FFmpeg args), tools.rs::media_recipes_publish_verified_outputs_with_lineage_and_undo (name, role, undo); the Mac adapter's location stripping is not asserted (`AK::commonRecipesWriteVerifiableOutputs` checks codecs) Round 17: the Mac adapter location stripping is pinned by platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaVideoExportTests.remuxCopiesStreamsAndStripsLocation (stays with FileIDAppleKit). |
| CommonMediaTools | trimKeepsTheIntervalAndAudioSync (keyframe, exact) | Trim keeps the requested interval and A/V sync, copies for keyframe and re-encodes for exact, and refuses an end past the file. | a | media_recipes.rs::verification_rejects_...; source_checks_refuse_what_cannot_be_done_faithfully (TRIM_PAST_END); tools.rs::media_recipes_publish_verified_outputs...; AK::commonRecipesWriteVerifiableOutputs (duration, h264) |
| CommonMediaTools | audioExtractionWritesAACAndRefusesSilentVideo | Audio extraction writes AAC M4A registered as audio; a silent video is refused. | a | media_recipes.rs::source_checks_refuse_... (NO_AUDIO); verification test (audio case); tools.rs::media_recipes_publish_verified_outputs... ("audio" kind); AK::commonRecipesWriteVerifiableOutputs |
| CommonMediaTools | proxyIsSmallH264LinkedToItsSourceAndKeepsOrientation | A proxy is H.264/AAC, at most 640 px, orientation-correct and linked to its source with role "proxy". | a | media_recipes.rs::verification_rejects_... (proxy portrait); tools.rs::media_recipes_publish_verified_outputs... (role proxy); AK::commonRecipesWriteVerifiableOutputs |
| CommonMediaTools | rotationChangesOrientationWithoutReencoding (4 cases) | Rotate adds the angle to the existing rotation, copies codecs and gives the right landscape or portrait shape. | a + c (round 17; was a*) | media_recipes.rs::arguments_copy_losslessly_... (270 on portrait, 90) and verification test; the Mac adapter is tested for 90 on a landscape clip only (AK); the 180 and 270 and portrait compositions are not Round 17: 90, 180 and 270 on a portrait clip are pinned by platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaVideoExportTests.rotationAddsToAPortraitSourcesRotationWithoutReencoding. |
| CommonMediaTools | rotatingNinetyRightMovesTheTopLeftMarkerToTheTopRight | A 90 degree rotation is clockwise: the top-left marker ends up top right. | c (round 17; was d) | Mac adapter `MediaExport.common` stays; ported to platforms/apple/Tests/AppleKitTests/MediaPlanTests.swift::MediaVideoExportTests.rotatingNinetyRightMovesTheTopLeftMarkerToTheTopRight |
| CommonMediaTools | lossyAndLosslessRecipesRefuseUnsupportedSourcesBeforeWriting | A non-media or unreadable source is refused before anything is written, and no operation is recorded. | a+b | media_recipes.rs::source_checks_refuse_...; tools.rs::media_recipes_refuse_before_any_output_without_the_vetted_runtime; media-tools-recipe-validation-and-refusals ("non-media source", "nothing written", "no plans") |

### Totals
117 tests across 18 suites (matches grep -c per file). By category: a 56, a* 14, a+b 7, b 3, c 18, d 19. The `a+b` rows are pinned by both a Rust test and a conformance case; `a*` rows are pinned except for the sub-invariant named in Gaps.

### Gaps (d and partial a*), ranked
Order: data loss or wrong file operations, then wrong results, then presentation. Mac-adapter gaps suggest a Swift test in `platforms/apple/Tests/AppleKitTests/MediaAdapterTests.swift`, because the code lives in `FileIDAppleKit`.

### Data loss or wrong file operations
1. **MediaTools recovery with a symlink alias (a\*).** Regression: cleanup follows a `.FileIDExport-<uuid>.part` symlink and deletes the user's source file. Home: `E/commands/tools.rs` tests. Shape: extend `restart_cleanup_preserves_unrelated_files_and_sources` to plant a uuid-named symlink to the source in `stage_paths`, run `recover`, assert the source bytes and the link remain.
2. **Mac video export execute and undo once the Swift engine is deleted (b, at risk).** The `cross-video-*` and `cross-media-*` cases need Swift as peer. Regression: the hosted Rust engine writes, publishes or undoes a video with no test that runs. Home: convert them to single-engine `rust` cases in `shared/test-corpus/conformance/video-tools.json` (preview, execute, source sha unchanged, undo removes the output), as the retirement doc says.
3. **Chapter export after the chapters change (a\*).** Regression: an old preview exports edited or deleted chapter markers (tools.rs:561). Home: `tools.rs`. Shape: preview a chapters export, save a changed chapter, execute, assert error and no file. Closed round 17: `tools.rs::tests::a_chapter_export_refuses_markers_edited_added_or_deleted_after_its_preview`.
4. **Passages and coverage not marked stale when the source changes.** Regression: old transcripts and sampled-frame captions keep appearing in Chat search for a replaced file. Home: `E/db/migrations.rs` (or `commands/catalog.rs`). Shape: insert a passage and a coverage row for a file, change the file's size, assert `stale=1` and coverage `stale`. Closed round 17: `commands/catalog.rs::tests::passages_observations_and_coverage_go_stale_when_the_source_changes`.
5. **Operation recovery state (a\*).** Regression: an interrupted export stays "running" forever. Home: `tools.rs`. Shape: after `recover`, assert `catalog_operations.state='failed'` (the stage test already inserts the row).

### Wrong results or privacy
6. **Photo EXIF orientation and metadata stripping on a real JPEG.** Regression: exported photos keep GPS or are rotated wrongly. Home: `tools.rs`. Shape: write a JPEG with orientation 6 plus GPS and a user comment, export, assert dimensions swap and no GPS or comment in the output bytes. Closed round 17: `tools.rs::tests::a_photo_export_applies_the_exif_orientation_and_keeps_no_location`.
7. **Video and remux export on the Mac adapter: location stripped, audio kept, duration, all frame ratios and sizes, pad not stretch, enlargement.** Regression: location metadata leaks, audio is lost, or a 4:5 or 1920 export has the wrong size. Home: `AppleKitTests/MediaAdapterTests.swift`. Shape: location-tagged clip, export each ratio and size, probe: no location key, AAC present, duration within 0.1 s, ratio within 0.02, centre pixel bright and edge pixel black for pillarbox, enlargement beyond source size. Closed round 17 as Swift tests that stay: `AppleKitTests/MediaPlanTests.swift` (MediaVideoExportTests).
8. **Follow-people crop plan (MediaReframing.plan).** Regression: people are cut out of a 9:16 crop, or a jumpy crop is published. Home: `AppleKitTests`. Shape: port the two plan tests (contains every person, ratio, smooth step under 40 px, fit on lost or unsafe group, invalid boxes and times); add a moving-subject export check. Closed round 17: ported to `AppleKitTests/MediaPlanTests.swift` (MediaReframingPlanTests).
9. **Stabilization alignment and smoothing.** Regression: black borders, over-zoom or added jitter in published videos. Home: `AppleKitTests`. Shape: port the registration test and the smoothing/corner-inside-frame test; keep a coarse jitter-reduction export test. Closed round 17: ported to `AppleKitTests/MediaPlanTests.swift` (MediaStabilizationPlanTests).
10. **Rotate direction and composition on the Mac path.** Regression: a "rotate right" turns left, or portrait sources rotate wrongly. Home: `AppleKitTests`. Shape: marker clip, rotate 90, assert top-left marker lands top right; rotate 180 and 270 on a portrait clip and check the rotation value. Closed round 17: `AppleKitTests/MediaPlanTests.swift` (rotate right, and 90/180/270 on a portrait clip).
11. **Speech passage edge cases (a\*).** Regression: a NaN timestamp or infinite confidence is stored; a passage ends after the video. Home: `E/pipeline/speech_passages.rs`. Shape: feed NaN timestamp, infinite confidence, and a word ending past `media_duration`; assert dropped or clamped. Closed round 17: `speech_passages.rs::tests::non_finite_words_are_dropped_and_a_passage_never_ends_after_the_video`.
12. **`supports()` refusal matrix.** Regression: an unsupported recipe is accepted and fails later. Home: `tools.rs::enlargement_flags_...`. Shape: assert false for ratio "10:3", framing "unknown", photo with aspect or framing or stabilization, and true for the supported combinations.
13. **Chapter suggestions abstain on similar captions.** Regression: spurious draft chapters in a single-scene video. Home: `E/pipeline/timeline_analysis_tests.rs`. Shape: three captions of one scene, assert empty. Also assert drafts are unedited, not stale, confidence 0.2.
14. **`moment_chapters` with NaN or reversed frame times.** Regression: a chapter with NaN bounds is written. Home: `timeline_analysis_tests.rs`. Shape: call with `[NaN]` and reversed times, assert empty.
15. **Moment window write for a job no longer running (a\*).** Regression: a paused job still writes. Home: `E/commands/timeline_tests.rs`. Shape: set the job paused and write a window, assert error and no coverage row.
16. **Chat summary model id (ChatModelWake).** Regression: an IPC-supplied unknown model kind is loaded. Home: `E/commands/chat.rs`. Shape: `summary_model(Some("hf:attacker/arbitrary"))` is `None`; unknown kind with `use_model` falls back to the plain answer.
17. **Chat reply labels (a\*).** Regression: an unverified person appearance is shown as confirmed; the event and time are missing from the reply. Home: `catalog_retrieval.rs` and `chat.rs`. Shape: add a non-edited observation and assert "unverified appearance"; assert the reply message names the event and "2:30".

### Process hygiene and presentation
18. **Mac worker timeout and cancel (`native_process::self_worker::run`).** Regression: a stuck or cancelled video worker keeps running and holds CPU and files. Home: `E/models/native_process.rs`. Shape: run `/bin/sleep 30` through the worker launcher with a 1 s timeout and with the cancel flag set; assert the child is reaped and an error returns within 5 s. Still open round 17 (apple-adapters only; see the row).
19. **Describe Files yields to a waiting chat summary (ChatYield, c).** Regression: Chat waits behind a long Describe run. Home: `E/commands/deep_analyze_mlx.rs`. Shape: with a waiter on `Resident::shared()`, assert the loop stops between files. `resident_owner_tests` covers `has_waiters` only.
20. **Weighted rank fusion in Chat (`chat::fuse`).** Regression: "or" and multi-subject questions return hits in a different order or lose the chapter id. Home: `E/commands/chat.rs`. Shape: port the Swift test (`[[2,1c,1],[1,3]]` gives `[1,2,1c,3]`).
21. **Whitespace-only chat request (a\*).** Regression: a blank question writes history. Home: `chat.rs::invalid_requests_cannot_write_history_or_change_files`. Shape: add `" "` to the text list. Closed round 17.
22. **History block before the request in the model prompt.** Regression: the model sees history after the request. Home: `chat.rs`. Shape: build the prompt string via the same function used at lines 230-238 and assert the prefix "Earlier in this conversation:".
23. **Stored recipe for a stabilized export.** Regression: undo or re-run loses the recipe. Home: `tools.rs`. Shape: after execute, `SELECT recipe_json FROM catalog_assets` deserialises equal to the request recipe. Closed round 17: `tools.rs::tests::an_export_stores_the_recipe_it_ran_beside_its_lineage`.
24. **Moment-reply 16 KB cap.** Regression: an oversized model reply is accepted. Home: `timeline_analysis_tests.rs`. Shape: `parse_moments` with a 16 385-byte reply returns an error. Closed round 17: `timeline_analysis_tests.rs::a_moment_reply_over_sixteen_kilobytes_is_refused`.


## Rust tests that name a Swift file or the Swift engine in a comment

These comments go stale when the Swift engine is deleted. Counted over comment lines inside test code (`#[cfg(test)]` modules, `*_tests.rs`, `tests/`) under `platforms/windows/src/engine`: 31 files, 66 comment lines that say "Swift", 8 of them naming a `.swift` file. The most: `commands/timeline.rs` 8, `ipc/mod.rs` 6, `paths.rs` 6, `commands/chat_selection.rs` 5, `commands/catalog_take_auto.rs` 4, `pipeline/restructure_semantic.rs` 4. Method: a script that walks `src/**/*.rs` and `tests/*.rs`, starts at the first `#[cfg(test)]` line (or the top of a `*_tests.rs` file), and counts lines starting with `//`, `*` or `/*` that match `Swift` or `\w+\.swift`. A blunt cross-check over all comments: `grep -rnE '\.swift' platforms/windows/src/engine/src platforms/windows/src/engine/tests | grep -E '//'` gives 55 lines (tests and non-test code together).
