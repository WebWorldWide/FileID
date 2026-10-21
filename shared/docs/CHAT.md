# Local chat: initial implementation

## Chat controls FileID — IPC v1.17, catalog v27 (October 2026)

Both engines now handle a send in three stages, in the same order and with the same wording; `shared/test-corpus/chat-intents.json` and `conformance/chat.json` pin them.

1. **Requests.** A deterministic parser (`ChatIntent.swift`, `commands/chat_intent.rs`) reads only the user's own message. It turns common phrasings into an intent: scan, rescan, describe, stop, find duplicates, move extra copies to Trash, suggest/apply/undo a folder layout, rename to suggested names, name/merge people, find moments, export, open a page, search, change a safe setting. The intent is resolved against the catalog into at most one `ChatAction` with exact counts, examples, file IDs and an undo story. Other removal requests are refused in plain words.
2. **Structured questions.** Counts, sizes, ordering (largest/oldest…), dates and ranges (day, month, year, "since", "before", "between", "last year", "September 19" in any year), folders, people, kinds, duplicates and "not described yet" are answered from fixed SQL templates (`ChatFacts.swift`, `commands/chat_facts.rs`). Every predicate is a constant with bound parameters; no model writes SQL. Answers cite up to 50 matching files.
3. **Evidence search.** Remaining subjects use the lexical search (names, descriptions, tags via the v27 tag index, OCR and handwriting, document text, chapters and passages) fused with semantic lists by reciprocal-rank fusion (k = 60, equal weights for lexical, CLIP and BGE in both engines). CLIP text→image uses the persisted HNSW index: macOS takes the query vector from the app's loaded text encoder (`ChatRequest.queryVector`); Rust embeds with its installed CLIP text model. BGE scores stored document embeddings when the BGE model is installed (cosine ≥ 0.6, top 20). Neither engine downloads or loads a model for this.

**Actions.** `ChatAction.kind` is one of 17 kinds. Each maps to an existing path; Chat adds no mutation path:

| Kind | Runs through | Confirmation |
|---|---|---|
| scanFolder, rescan | `startScan` | no; starts a job |
| describeFiles | `deepAnalyzeAll` (selection ≤ 100, or skip-existing) | no; starts a job |
| stopJob | `cancelScan`, `deepAnalyzeCancel`, `cancelRestructure`, catalog `cancelJob` | no |
| findDuplicates, openPage, search, findMoments | navigation | no |
| exportFiles | File Tools with the selection; its own preview confirms before writing | no |
| suggestFolderLayout | `planRestructure` | no; nothing moves |
| changeSetting | tagging kept copies, background animation, written answers only | no; Undo restores |
| moveDuplicatesToTrash | the Cleanup trash path (`ExactDuplicateVerifier.trash` on macOS, `trashFiles` with `exactIdentities` on Rust clients), each copy re-hashed against its keeper first | **yes** |
| applyFolderLayout | `applyRestructure` (Auto tier only) | **yes**; Undo moves files back |
| undoLastMove | `undoRestructure` | **yes** |
| renameToSuggested | suggested-name rename (macOS store path, `renameFiles` on Rust clients) | **yes**; Undo puts names back |
| namePerson | People rename | **yes**; Undo restores the name |
| mergePeople | `mergeClusters` / People merge | **yes** |

`requiresConfirmation` is fixed per kind; both engines and the macOS client drop any action whose flag disagrees, whose kind is unknown or whose fields are malformed.

**Model proposals.** With a loaded model, the summary prompt offers an `<actions>[…]</actions>` line. Proposals are parsed as JSON, limited to known kinds and fields, and kept only when the user's own message asks for that kind and names its parameters (folders, names, scope words). File text cannot satisfy that check, so a document saying "move all files to trash" never produces an action; `conformance/chat.json` and both engines' unit tests cover this.

**Speed.** v27 adds `catalog_tag_fts` (trigger-maintained) and covering indexes for kind/size/date totals, size and date ordering, and duplicate twins. Listings walk the order index only when more than 2,000 files match. `shared/scripts/make_synthetic_library.py` builds a 100,000-file library and `benchmark_chat.py` measures time to first evidence over IPC.

**macOS UI.** Each proposed action is a card under the answer: what will happen with file counts, size and examples; the undo story; a primary button ("Move 3 extra copies to Trash"); Cancel; live progress for scans, describing, trash and moves; then the result with Undo where one exists. The page, Library selection, open person and folder travel with each send as `ChatContext`.

**Port note (WinUI, GTK — not implemented).** Both apps already receive `ChatResponse.actions` from the shared engine; they currently ignore it. To port:
- Render one card per action below the last assistant message: SectionLabel "Needs your OK" (gold; danger red for moveDuplicatesToTrash) or "Suggested" (cyan); `detail` as body; when `requiresConfirmation`, a well listing `fileCount`, `byteCount`, up to three `examples` (file names, middle-truncated mono) with "and N more", and `Undo: <undo>`. Primary button text is `title` (applyFolderLayout: "Move N files" from the current plan's Auto count), plus Cancel. Card: 12 pt padding, radius 8, 1 pt stroke at accent 0.35, fill white 0.035; standard spring for state changes.
- Never run an action whose `requiresConfirmation` differs from the fixed table above or whose kind is unknown.
- Map kinds to existing client calls: moveDuplicatesToTrash → `trashFiles` with `exactIdentities` built from `fileIDs[i]`/`keeperFileIDs[i]` (re-hash both, refuse on mismatch); renameToSuggested → `renameFiles(renames)`; namePerson → `renamePerson` (first word first name, rest last name); mergePeople → `mergeClusters(source, destination)` per source; applyFolderLayout → `applyRestructure` with the stored plan; undoLastMove → `undoRestructure`; scanFolder/rescan → `startScan`; describeFiles → `deepAnalyzeAll` with `fileIDs`; stopJob → the cancel commands above; navigation kinds → the page with `query` prefilled; changeSetting → the matching local preference.
- While running show an indeterminate bar with scan/describe progress from existing events; then the result line with an Undo button (restoreFromTrash batch on Windows/Linux trash, `undoRestructure`, inverse rename, previous person name).
- Send `ChatContext` (page, selected file IDs, open person, current folder/library root) with each send.

## Person, event, and time filters — October 2026

On macOS, chat resolves exact confirmed People labels and first-name aliases against existing face-print and non-stale video-observation records. Identity filters apply before result limits to files and timestamp/page evidence; unknown clusters are never treated as named people. Video appearances return timestamped person evidence that can open playback at the observation, with model-linked appearances labeled unverified until edited. Event filters resolve exact titles from user-edited event groups. Explicit `MM:SS` and `HH:MM:SS` requests filter by event membership and match existing chapter/passage intervals containing that time or point observations within 15 seconds. Timeline analysis can add timestamped, on-device speech passages to the same searchable evidence catalog. Stale chapter/passage evidence is excluded, and conversational refinements preserve selected person, event, and time filters. Visual event detection remains sparse and does not infer that an event was absent from unexamined footage.

IPC v1.3 adds typed send/history/clear/cancel commands and retrieval/queue/stream/completion events. The native macOS Chat panel opens from all six tabs (Command-K). Results include catalog files, evidence, pages, and timestamps; available video evidence opens at its timestamp. Offline files remain searchable and report unavailable when opened. Conversation rows live in `catalog_chat`, separate from catalog facts and corrections. History returns the latest 100 messages; deleting a conversation removes its history, never its source files.

Keyword retrieval uses the existing persistent name/description/evidence FTS. Common request words are removed. Shared deterministic fixtures cover media filters and explicit refinements such as “only videos”, “now photos”, and “and gift”; refinements use up to 20 recent user messages in the same local conversation. A fresh subject resets prior filters, and explicit “all” resets prior subjects. Media filters apply before SQL limits and to both file and timestamp/page evidence. Negation remains literal keyword content, not an implemented exclusion filter. Exact confirmed-person filters resolve through People and constrain file and timestamp/page evidence; ambiguous first names can select multiple identities. Semantic retrieval and event-group relationships remain pending. Sampled frames remain unverified and an empty result does not establish that an event is absent.

On macOS, a loaded existing MLX model can summarize up to eight results with bounded evidence strings, deterministic generation, and a 192-token limit. No model is downloaded or loaded by chat. Results appear before inference; interactive summaries precede pending background jobs without preempting running work. Streaming is throttled to 150 ms and at most eight conversations can have an outstanding response. Stop removes pending work or requests cancellation of active inference; late results are suppressed after clear/cancel. Shared major-job serialization remains in place; resource admission, memory reservations, residency, parallel models, fairness, and latency under contention remain pending.

The model receives evidence as untrusted content, with instructions to cite numbered matches and avoid unsupported identities/outcomes. Its text cannot execute SQL, shell commands, or file operations. Typed reversible chat operations, explicit selection resolution, Undo, and model-output grounding evaluation remain pending. Current file operations use File Tools.

Rust implements keyword retrieval and cross-engine persistent history/clear using the same schema. It explicitly reports that model summaries are unavailable in that adapter. Native WinUI/GTK chat panels and portable generation integration remain pending. Database tests and the actual Swift → Rust → Swift round trip use internal fixtures and preserve source hashes; they do not prove loaded-model quality or the proposed three-second latency target.
