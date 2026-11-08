import Foundation
import GRDB
import FileIDShared

actor ChatService {
    static let shared = ChatService()
    var active: [String: String] = [:]
    private var running: String?
    private var runningInterleaved = false
    private var accumulated: [String: String] = [:]
    private var lastEmit: [String: Date] = [:]
    var latestRequest: [String: String] = [:]

    /// Reciprocal-rank fusion constant and per-list weights (lexical, CLIP,
    /// BGE). Identical in Rust `commands::chat`.
    static let rrfK = 60.0
    static let rrfWeights = [1.0, 1.0, 1.0]
    /// BGE-small cosine below which a document is not offered as evidence.
    static let bgeFloor: Float = 0.6
    static let bgeQueryPrefix = "Represent this sentence for searching relevant passages: "

    /// What a send found, before anything is written.
    struct Prepared: Sendable {
        var message: String
        var hits: [CatalogHit]
        var actions: [ChatAction] = []
        /// Whether a loaded model may summarize the evidence.
        var summarize: Bool
    }

    /// Same wording as the Rust engine so both apps tell the user what is happening.
    enum Busy: Error, LocalizedError {
        case conversation, tooMany
        var errorDescription: String? {
            switch self {
            case .conversation: "A local model summary is still running for this conversation."
            case .tooMany: "Too many local model summaries are running."
            }
        }
    }

    func handle(_ request: ChatRequest, database: Database, sink: IPCSink) async {
        do {
            guard !request.requestID.isEmpty, request.requestID.count <= 200,
                  !request.conversationID.isEmpty, request.conversationID.count <= 200,
                  Self.validContext(request) else { throw CatalogStore.InvalidRequest() }
            switch request.action {
            case "history":
                await emit(request, status: active[request.conversationID] == nil ? "completed" : "queued", message: active[request.conversationID] == nil ? "Local conversation history." : "A local model summary is still queued or running.", database: database, sink: sink)
            case "clear", "cancel":
                latestRequest[request.conversationID] = request.requestID
                if let id = active.removeValue(forKey: request.conversationID) {
                    await JobQueue.shared.cancelPending(id: "chat-" + id)
                    _ = await ChatYield.shared.remove(id: id)
                    if running == id {
                        // An interleaved summary runs between a Describe batch's files;
                        // the batch-wide cancel flag would stop that batch too.
                        if runningInterleaved { await DeepAnalyze.shared.cancelCurrentAnalysis() } else { await DeepAnalyze.shared.requestCancel() }
                    }
                    accumulated.removeValue(forKey: id); lastEmit.removeValue(forKey: id)
                }
                if request.action == "clear" {
                    try await database.pool.write { db in try db.execute(sql: "DELETE FROM catalog_chat WHERE conversation_id=?", arguments: [request.conversationID]) }
                }
                await emit(request, status: request.action == "clear" ? "completed" : "cancelled", message: request.action == "clear" ? "Conversation deleted." : "Model response stopped.", database: database, sink: sink)
            case "send":
                if active[request.conversationID] != nil { throw Busy.conversation }
                if active.count >= 8 { throw Busy.tooMany }
                guard let text = request.text,
                      !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, text.count <= 2000 else { throw CatalogStore.InvalidRequest() }
                if let rootID = request.context?.rootID { _ = try await database.pool.read { db in try LibraryRoots.Scope.load(db, id: rootID) } }
                if request.scopeFileIDs != nil {
                    try await selection(request, text: text, database: database, sink: sink)
                    return
                }
                active[request.conversationID] = request.requestID
                latestRequest[request.conversationID] = request.requestID
                try await database.pool.write { db in
                    try db.execute(sql: "INSERT INTO catalog_chat(id,conversation_id,role,text,created_at) VALUES(?,?,'user',?,?)", arguments: [UUID().uuidString, request.conversationID, text, Date().timeIntervalSince1970])
                }
                let prepared = try await Self.prepare(request, text: text, database: database)
                guard active[request.conversationID] == request.requestID else { return }
                if !prepared.summarize {
                    try await Self.save(prepared.message, role: "assistant", conversation: request.conversationID, database: database)
                    guard active[request.conversationID] == request.requestID else { return }
                    active.removeValue(forKey: request.conversationID)
                    await emit(request, status: "completed", message: prepared.message, hits: prepared.hits, actions: prepared.actions, database: database, sink: sink)
                    return
                }
                let hits = prepared.hits
                let explanation = prepared.message
                await emit(request, status: "retrieving", message: explanation, hits: hits, database: database, sink: sink)
                guard active[request.conversationID] == request.requestID else { return }
                var wakeFailed = false
                var readyModel: AIModelKind?
                if request.useModel == true, !hits.isEmpty {
                    if case .ready(let loaded) = await DeepAnalyze.shared.loadState, request.modelKind == nil || request.modelKind == loaded.rawValue {
                        readyModel = loaded
                    } else if let kind = Self.wakeableKind(request), await Self.canWake() {
                        await emit(request, status: "queued", message: "Results are ready. Waking \(kind.displayName) to write the answer; the first time takes a few seconds.", hits: hits, database: database, sink: sink)
                        do {
                            try await DeepAnalyze.shared.ensureLoaded(kind: kind)
                            if case .ready(let loaded) = await DeepAnalyze.shared.loadState, loaded == kind { readyModel = loaded }
                        } catch {
                            JSONLog.shared.warn(ev: "chat_model_wake_failed", error: "\(error)")
                            wakeFailed = true
                        }
                        guard active[request.conversationID] == request.requestID else { return }
                    }
                }
                if let model = readyModel {
                    active[request.conversationID] = request.requestID
                    let busy = await JobQueue.shared.hasActive(category: .deepAnalyze)
                    let wait = busy ? "Results are ready. The local model will write its answer as soon as the file it is describing is done." : "Results are ready. The local model is writing its answer; a model that was resting can take a few seconds to wake."
                    await emit(request, status: "queued", message: wait, hits: hits, database: database, sink: sink)
                    guard active[request.conversationID] == request.requestID else { return }
                    let job: @Sendable (Bool) async -> Void = { interleaved in
                        await ChatService.shared.summarize(request, hits: hits, model: model, fallback: explanation, database: database, sink: sink, interleaved: interleaved)
                    }
                    if busy { await ChatYield.shared.park(id: request.requestID, job) }
                    await JobQueue.shared.enqueue(.init(id: "chat-" + request.requestID, category: .deepAnalyze, title: "Chat evidence summary", etaSeconds: nil, priority: .interactive, blocksExclusiveCommands: false) {
                        if busy, !(await ChatYield.shared.remove(id: request.requestID)) { return }
                        await job(false)
                    })
                } else {
                    var loading = false
                    if case .loading = await DeepAnalyze.shared.loadState { loading = true }
                    let modelNote = hits.isEmpty ? "" : (wakeFailed ? " The local AI model couldn't be started, so these are the matching files." : loading ? " The local AI model is still loading, so these are the matching files; ask again in a moment for a written answer." : " Load a local AI model for a written answer; these are the matching files.")
                    let message = explanation + (request.useModel == true ? modelNote + " Nothing was downloaded." : "")
                    try await Self.save(message, role: "assistant", conversation: request.conversationID, database: database)
                    guard active[request.conversationID] == request.requestID else { return }
                    active.removeValue(forKey: request.conversationID)
                    await emit(request, status: "completed", message: message, hits: hits, database: database, sink: sink)
                }
            default: throw CatalogStore.InvalidRequest()
            }
        } catch {
            if active[request.conversationID] == request.requestID { active.removeValue(forKey: request.conversationID) }
            await emit(request, status: "error", message: error.localizedDescription, database: database, sink: sink)
        }
    }

    /// The model the user picked for Chat, when it is already installed. Chat never downloads.
    static func wakeableKind(_ request: ChatRequest) -> AIModelKind? {
        guard let raw = request.modelKind, let kind = AIModelKind(rawValue: raw), kind.isInstalledOnDisk() else { return nil }
        return kind
    }

    /// Loading swaps the resident model, so it waits for a moment when no Describe or video job is running.
    static func canWake() async -> Bool {
        let busy = await DeepAnalyze.shared.isAnalyzing
        let queued = await JobQueue.shared.hasActive(category: .deepAnalyze)
        return !busy && !queued
    }

    static func validContext(_ request: ChatRequest) -> Bool {
        if (request.queryVector == nil) != (request.embeddingModel == nil) { return false }
        guard let context = request.context else { return true }
        func idsOK(_ ids: [Int64]?, _ limit: Int) -> Bool { ids.map { $0.count <= limit && $0.allSatisfy { $0 > 0 } } ?? true }
        func pathOK(_ path: String?) -> Bool { path.map { !$0.isEmpty && $0.count <= 32_768 } ?? true }
        return idsOK(context.fileIDs, ChatActions.maxIDs) && idsOK(context.personIDs, 50)
            && (context.page.map(ChatAction.pages.contains) ?? true)
            && pathOK(context.folder) && pathOK(context.libraryRoot) && (context.rootID.map { $0 > 0 } ?? true)
    }

    /// The search plan a message implies, with its structured part.
    private static func searchPlan(_ text: String, previous: ChatSearchPlan?, people: [ChatSearchPlan.KnownPerson], events: [ChatSearchPlan.KnownEvent]) -> (ChatIntent.Question, ChatSearchPlan) {
        let question = ChatIntent.question(text)
        let plan = ChatSearchPlan.resolve(question.reduced, previous: previous, knownPeople: people, knownEvents: events)
        // "how many are from 2018?" names nothing of its own: it narrows the last search.
        let bare = plan.query.isEmpty && plan.kinds.isEmpty && plan.personIDs.isEmpty && plan.eventIDs.isEmpty && plan.excludedPersonIDs.isEmpty
        let lower = Set(text.lowercased().split(whereSeparator: { !$0.isLetter }).map(String.init))
        // A duplicates question is about the library unless it points back ("duplicates among those").
        let pointsBack = !lower.isDisjoint(with: ["those", "these", "them", "they", "ones"])
        if let previous, bare, question.structured, !["overview", "folders", "formats"].contains(question.measure),
           !question.duplicates || pointsBack,
           lower.isDisjoint(with: ["files", "file", "library", "everything", "all", "total", "overall", "collection"]) {
            return (question, ChatSearchPlan.resolve(question.reduced, previous: previous, knownPeople: people, knownEvents: events, followUp: true))
        }
        return (question, plan)
    }

    /// Parses requests and questions and gathers evidence. Reads only.
    static func prepare(_ request: ChatRequest, text: String, database: Database) async throws -> Prepared {
        let root = request.context?.rootID == nil ? nil : try await database.pool.read { db in try LibraryRoots.Scope.load(db, id: request.context?.rootID) }
        let stage = try await database.pool.read { db -> (Prepared?, ChatSearchPlan, ChatIntent.Question, [CatalogHit]) in
            let knownPeople = try knownPeople(db)
            let knownEvents = try knownEvents(db)
            if let intent = ChatIntent.parse(text) {
                let context = ChatActions.Context(requestID: request.requestID, text: text, chat: request.context, people: try ChatActions.people(db),
                                                  knownPeople: knownPeople, knownEvents: knownEvents, source: "parser", root: root)
                let resolution = try ChatActions.resolve(db, intent, context)
                return (Prepared(message: resolution.message, hits: resolution.hits, actions: resolution.actions, summarize: false), ChatSearchPlan(query: "", kinds: []), ChatIntent.Question(), [])
            }
            let previousTexts = try String.fetchAll(db, sql: "SELECT text FROM (SELECT rowid,text FROM catalog_chat WHERE conversation_id=? AND role='user' ORDER BY rowid DESC LIMIT 21) ORDER BY rowid", arguments: [request.conversationID]).dropLast()
            let previous = previousTexts.filter { ChatIntent.parse($0) == nil }
                .reduce(nil as ChatSearchPlan?) { searchPlan($1, previous: $0, people: knownPeople, events: knownEvents).1 }
            let (question, plan) = searchPlan(text, previous: previous, people: knownPeople, events: knownEvents)
            var structured = ChatFacts.Filter(kinds: plan.kinds, personIDs: plan.personIDs, personNames: plan.personNames,
                                              date: question.date, folder: question.folder, undescribed: question.undescribed,
                                              extensions: question.formats ?? [], allPeople: plan.allPeople,
                                              excludedPersonIDs: plan.excludedPersonIDs, excludedPersonNames: plan.excludedPersonNames,
                                              minBytes: question.minBytes, maxBytes: question.maxBytes, root: root)
            // Counts, sizes, orders and duplicates must see every match, so a
            // subject becomes the full set of files it describes.
            let wantsAll = question.measure != "list" || question.explicitOrder || question.duplicates
            if question.structured, plan.eventIDs.isEmpty, plan.timeSeconds == nil, plan.query.isEmpty || wantsAll {
                if !plan.query.isEmpty, let ids = try ChatFacts.subjectIDs(db, plan.query) {
                    structured.ids = ids
                    structured.subject = plan.query
                }
                let (message, hits) = try ChatFacts.answer(db, question, structured)
                return (Prepared(message: message, hits: hits, summarize: false), plan, question, [])
            }
            let groups = ChatFacts.subjectGroups(plan.query)
            let hits = groups.count > 1
                ? fuse(try groups.map { try CatalogStore.search(db, query: $0.joined(separator: " "), kinds: plan.kinds, personIDs: plan.personIDs, eventIDs: plan.eventIDs, timeSeconds: plan.timeSeconds, root: root) })
                : try CatalogStore.search(db, query: plan.query, kinds: plan.kinds, personIDs: plan.personIDs, eventIDs: plan.eventIDs, timeSeconds: plan.timeSeconds, root: root)
            return (nil, plan, question, hits)
        }
        if let done = stage.0 { return done }
        let (_, plan, question, lexical) = stage
        var hits = lexical
        if !plan.query.isEmpty {
            let extra = await semantic(request, query: plan.query, database: database)
            if !extra.isEmpty {
                let scope = ChatFacts.Filter(kinds: plan.kinds, personIDs: plan.personIDs, root: root)
                var lists = [lexical]
                for list in extra { lists.append(try await database.pool.read { db in try ChatFacts.restrict(db, list, scope) }) }
                hits = fuse(lists)
            }
        }
        var extraFilter: ChatFacts.Filter?
        if question.structured || plan.allPeople || !plan.excludedPersonIDs.isEmpty {
            extraFilter = ChatFacts.Filter(personIDs: plan.allPeople ? plan.personIDs : [], date: question.date, folder: question.folder,
                                           undescribed: question.undescribed, extensions: question.formats ?? [], allPeople: plan.allPeople,
                                           excludedPersonIDs: plan.excludedPersonIDs, excludedPersonNames: plan.excludedPersonNames,
                                           minBytes: question.minBytes, maxBytes: question.maxBytes, root: root)
            let filter = extraFilter!
            let current = hits
            hits = try await database.pool.read { db in try ChatFacts.restrict(db, current, filter) }
        }
        // Search keeps its best hundred and matches words as written; fill a short
        // list from every file the subject describes (singulars too) that passes
        // the same filters.
        if hits.count < ChatFacts.shown, plan.eventIDs.isEmpty, plan.timeSeconds == nil, !plan.query.isEmpty || extraFilter != nil {
            var fill = extraFilter ?? ChatFacts.Filter()
            fill.kinds = plan.kinds
            fill.personIDs = plan.personIDs
            fill.allPeople = plan.allPeople
            fill.root = root
            let current = hits
            let base = fill
            let more = try await database.pool.read { db -> [CatalogHit] in
                var fill = base
                if !plan.query.isEmpty {
                    guard let ids = try ChatFacts.subjectIDs(db, plan.query) else { return [] }
                    fill.ids = ids
                }
                let seen = Set(current.map(\.fileID))
                let count = try ChatFacts.totals(db, fill).count
                return try ChatFacts.listed(db, fill, order: question.order, limit: ChatFacts.shown + seen.count, count: count)
                    .filter { !seen.contains($0.fileID) }
            }
            hits += more.prefix(ChatFacts.shown - hits.count)
        }
        let scope = plan.query.isEmpty ? "in the library" : "for “\(plan.query)”"
        let filter = plan.kinds.isEmpty ? "" : " (\(plan.kinds.map(kindLabel).joined(separator: ", ")))"
        let people = plan.personNames.isEmpty ? "" : " (people: \(plan.personNames.joined(separator: plan.allPeople ? " and " : ", ")))"
        let events = plan.eventNames.isEmpty ? "" : " (event: \(plan.eventNames.joined(separator: ", ")))"
        let time = plan.timeSeconds.map { " (near \(DurationText.clock($0)))" } ?? ""
        let structure = extraFilter.map { filter -> String in var shown = filter; shown.personNames = []; return shown.qualifiers }.flatMap { $0.isEmpty ? nil : " (\($0.trimmingCharacters(in: .whitespaces)))" } ?? ""
        let qualifiers = filter + people + events + time + structure
        let message = plan.query.isEmpty && plan.kinds.isEmpty && plan.personIDs.isEmpty && plan.eventIDs.isEmpty && plan.timeSeconds == nil
            ? "Add a subject, event, time, or media type such as videos or photos. No search was run."
            : hits.isEmpty
                ? "No matches \(scope)\(qualifiers). Try names or a few descriptive words; files that haven't been analyzed may still contain it."
                : "Found \(hits.count) \(hits.count == 1 ? "match" : "matches") \(scope)\(qualifiers)." + (hits.contains { $0.kind == "sampledFrame" } ? " Sampled-frame descriptions remain unverified." : "")
        return Prepared(message: message, hits: hits, summarize: true)
    }

    /// CLIP text→image through the persisted HNSW graph (the client supplies
    /// the query vector) and BGE over stored document embeddings when its
    /// model is installed. Never downloads.
    private static func semantic(_ request: ChatRequest, query: String, database: Database) async -> [[CatalogHit]] {
        var lists: [[CatalogHit]] = []
        if let vector = request.queryVector, request.embeddingModel == CLIPEmbeddingSpace.modelID {
            let search = CatalogRequest(requestID: request.requestID + "-visual", action: "search", searchMode: "semantic",
                                        queryVector: vector, embeddingModel: CLIPEmbeddingSpace.modelID, limit: 100, rootID: request.context?.rootID, resultScope: "files")
            if let response = try? await CatalogSearch.handle(search, database: database), response.status == "ok" {
                lists.append(response.hits)
            }
        }
        if BGETextService.isInstalledOnDisk, BGETextService.shared.load(modelDir: BGETextService.defaultModelDir),
           let wanted = BGETextService.shared.embed(bgeQueryPrefix + query) {
            let hits = (try? await database.pool.read { db in try bgeHits(db, wanted) }) ?? []
            if !hits.isEmpty { lists.append(hits) }
        }
        return lists
    }

    static func bgeHits(_ db: GRDB.Database, _ wanted: [Float]) throws -> [CatalogHit] {
        // Unreadable files drop out after scoring, so take a margin over the twenty shown.
        if let best = try BGEDocumentMatrix.shared.scores(db, wanted: wanted, floor: bgeFloor, model: "bge_small_en_v1_5", limit: 200) {
            guard !best.isEmpty else { return [] }
            let ids = String(decoding: try JSONEncoder().encode(best.map(\.1)), as: UTF8.self)
            let rows = try Row.fetchAll(db, sql: "SELECT f.id,f.path_text,COALESCE(substr(d.text,1,200),'') AS text FROM files f LEFT JOIN doc_text d ON d.file_id=f.id WHERE f.failed=0 AND f.id IN (SELECT value FROM json_each(?))", arguments: [ids])
            let byID = Dictionary(uniqueKeysWithValues: rows.map { ($0["id"] as Int64, $0) })
            return best.compactMap { score, id -> CatalogHit? in
                guard let row = byID[id] else { return nil }
                return CatalogHit(fileID: id, path: row["path_text"], kind: "documentText", text: row["text"], evidenceID: "documentText:\(id)")
            }.prefix(20).map { $0 }
        }
        var scored: [(Float, CatalogHit)] = []
        let rows = try Row.fetchCursor(db, sql: "SELECT e.file_id,e.embedding,f.path_text,COALESCE(substr(d.text,1,200),'') AS text FROM text_embeddings e JOIN files f ON f.id=e.file_id LEFT JOIN doc_text d ON d.file_id=e.file_id WHERE f.failed=0 AND e.model='bge_small_en_v1_5' AND length(e.embedding)=?", arguments: [wanted.count * 4])
        while let row = try rows.next() {
            let score = BGEDocumentMatrix.dot(row["embedding"], wanted)
            guard score.isFinite, score >= bgeFloor else { continue }
            let id: Int64 = row["file_id"]
            scored.append((score, CatalogHit(fileID: id, path: row["path_text"], kind: "documentText", text: row["text"], evidenceID: "documentText:\(id)")))
        }
        return scored.sorted { $0.0 != $1.0 ? $0.0 > $1.0 : $0.1.fileID < $1.1.fileID }.prefix(20).map(\.1)
    }

    /// Weighted reciprocal-rank fusion. Hits share a key per evidence item or
    /// file; ties keep first appearance. Mirrors Rust `chat::fuse`.
    static func fuse(_ lists: [[CatalogHit]]) -> [CatalogHit] {
        guard lists.count > 1 else { return lists.first ?? [] }
        var ranked: [(CatalogHit, Double)] = []
        var positions: [String: Int] = [:]
        for (listIndex, list) in lists.enumerated() {
            let weight = listIndex < rrfWeights.count ? rrfWeights[listIndex] : 1
            for (rank, hit) in list.enumerated() {
                let key = hit.evidenceID.map { "evidence:" + $0 } ?? "file:\(hit.fileID)"
                let score = weight / (rrfK + Double(rank) + 1)
                if let index = positions[key] { ranked[index].1 += score } else { positions[key] = ranked.count; ranked.append((hit, score)) }
            }
        }
        return ranked.indices.sorted { ranked[$0].1 != ranked[$1].1 ? ranked[$0].1 > ranked[$1].1 : $0 < $1 }.map { ranked[$0].0 }
    }

    static func query(_ text: String) -> String {
        ChatSearchPlan.resolve(text).query
    }

    static func knownPeople(_ db: GRDB.Database) throws -> [ChatSearchPlan.KnownPerson] {
        try Row.fetchAll(db, sql: "SELECT id,name,title,first_name,middle_name,last_name,suffix FROM persons WHERE COALESCE(is_unknown,0)=0")
            .map { row in
                let structuredName = [
                    row["title"] as String?,
                    row["first_name"] as String?,
                    row["middle_name"] as String?,
                    row["last_name"] as String?,
                    row["suffix"] as String?
                ].compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { !$0.isEmpty }
                    .joined(separator: " ")
                let legacyName: String? = row["name"]
                let firstName: String? = row["first_name"]
                let names = [structuredName, legacyName ?? "", firstName ?? ""]
                    .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { !$0.isEmpty }
                return ChatSearchPlan.KnownPerson(id: row["id"], names: Array(Set(names)))
            }
    }

    static func knownEvents(_ db: GRDB.Database) throws -> [ChatSearchPlan.KnownEvent] {
        try Row.fetchAll(db, sql: "SELECT id,title FROM catalog_events WHERE user_edited=1 AND length(trim(title))>0 ORDER BY title LIMIT 200")
            .map { row in ChatSearchPlan.KnownEvent(id: row["id"], names: [row["title"]]) }
    }

    static func kindLabel(_ kind: String) -> String {
        switch kind {
        case "image": "photos"
        case "video": "videos"
        case "doc": "documents"
        case "pdf": "PDFs"
        case "other": "other files"
        default: kind
        }
    }

    /// Numbered one-line evidence instead of JSON: about a third fewer prompt
    /// tokens (prefill dominates time to first word on 4B+ models), no empty
    /// fields, and small models no longer echo the structure back.
    static func historyBlock(_ turns: [(role: String, text: String)], maxTurns: Int = 4, maxChars: Int = 300) -> String {
        let recent = turns.suffix(maxTurns).compactMap { turn -> String? in
            let flat = turn.text.split(whereSeparator: \.isNewline).joined(separator: " ").trimmingCharacters(in: .whitespaces)
            guard !flat.isEmpty else { return nil }
            return "\(turn.role == "user" ? "User" : "Assistant"): \(flat.prefix(maxChars))"
        }
        return recent.isEmpty ? "" : "Earlier in this conversation:\n" + recent.joined(separator: "\n") + "\n"
    }

    /// `details` adds each file's capture date and recognised people, so an
    /// answer about "winter photos of Alex" can confirm both instead of denying them.
    static func evidencePrompt(request: String, hits: [CatalogHit], limit: Int = 8, history: String = "", details: [Int64: [String]] = [:]) -> String {
        let shown = hits.prefix(limit)
        var lines = [history + "Request: \(request)", "Matches found: \(hits.count)\(hits.count > shown.count ? " (showing \(shown.count))" : "")", "<evidence>"]
        for (index, hit) in shown.enumerated() {
            var where_: [String] = [hit.kind]
            if let s = hit.startSeconds { where_.append(String(format: "at %.0f s", s)) }
            if let page = hit.page { where_.append("page \(page)") }
            where_.append(contentsOf: details[hit.fileID] ?? [])
            let text = hit.text.split(whereSeparator: \.isNewline).joined(separator: " ").prefix(400)
            lines.append("[\(index + 1)] \(URL(fileURLWithPath: hit.path).lastPathComponent) (\(where_.joined(separator: ", "))): \(text)")
        }
        lines.append("</evidence>")
        return lines.joined(separator: "\n")
    }

    /// Capture date and named people for each file's evidence line.
    static func evidenceDetails(_ db: GRDB.Database, _ ids: [Int64]) throws -> [Int64: [String]] {
        guard !ids.isEmpty else { return [:] }
        let list = String(decoding: try JSONEncoder().encode(ids), as: UTF8.self)
        var details: [Int64: [String]] = [:]
        for row in try Row.fetchAll(db, sql: """
            SELECT id, date(COALESCE(created_at,modified_at),'unixepoch','localtime') AS day FROM files
            WHERE id IN (SELECT value FROM json_each(?)) AND COALESCE(created_at,modified_at) IS NOT NULL
            """, arguments: [list]) {
            details[row["id"], default: []].append("taken " + (row["day"] as String))
        }
        var people: [Int64: [String]] = [:]
        for row in try Row.fetchAll(db, sql: """
            SELECT DISTINCT fp.file_id, trim(p.name) AS name FROM face_prints fp JOIN persons p ON p.id=fp.person_id
            WHERE fp.file_id IN (SELECT value FROM json_each(?)) AND COALESCE(fp.excluded,0)=0
              AND COALESCE(p.is_unknown,0)=0 AND length(trim(COALESCE(p.name,'')))>0
            ORDER BY fp.file_id, name
            """, arguments: [list]) {
            people[row["file_id"], default: []].append(row["name"])
        }
        for (id, names) in people { details[id, default: []].append("people: " + names.joined(separator: ", ")) }
        return details
    }

    /// Validated model proposals the user's own message asks for, resolved
    /// like parsed requests.
    static func modelActions(_ request: ChatRequest, proposals: [ChatIntent], database: Database) async -> [ChatAction] {
        let text = request.text ?? ""
        let permitted = proposals.filter { ChatActions.permitted($0, userText: text) }
        guard !permitted.isEmpty else { return [] }
        return (try? await database.pool.read { db -> [ChatAction] in
            let context = ChatActions.Context(requestID: request.requestID, text: text, chat: request.context, people: try ChatActions.people(db),
                                              knownPeople: try knownPeople(db), knownEvents: try knownEvents(db), source: "model",
                                              root: try LibraryRoots.Scope.load(db, id: request.context?.rootID))
            return Array(try permitted.flatMap { try ChatActions.resolve(db, $0, context).actions }.prefix(1))
        }) ?? []
    }

    private func summarize(_ request: ChatRequest, hits: [CatalogHit], model: AIModelKind, fallback: String, database: Database, sink: IPCSink, interleaved: Bool = false) async {
        guard active[request.conversationID] == request.requestID else { return }
        running = request.requestID
        runningInterleaved = interleaved
        defer { running = nil; runningInterleaved = false; accumulated.removeValue(forKey: request.requestID); lastEmit.removeValue(forKey: request.requestID) }
        var answer = fallback
        if case .ready(let current) = await DeepAnalyze.shared.loadState, current == model {
            if !interleaved { await DeepAnalyze.shared.clearCancel() }
            let earlier: [(role: String, text: String)] = (try? await database.pool.read { db in
                try Row.fetchAll(db, sql: "SELECT role,text FROM (SELECT rowid,role,text FROM catalog_chat WHERE conversation_id=? ORDER BY rowid DESC LIMIT 5) ORDER BY rowid", arguments: [request.conversationID])
                    .map { (role: $0["role"] as String, text: $0["text"] as String) }
            }) ?? []
            let history = Self.historyBlock(Array(earlier.dropLast()))
            func generate(_ prompt: String) async -> String? {
                let result = await DeepAnalyze.shared.runCancellableAnalysis {
                    do {
                        let text = try await DeepAnalyze.shared.answerCatalog(prompt: prompt) { chunk in
                            await ChatService.shared.stream(request, chunk: chunk, hits: hits, database: database, sink: sink)
                        }
                        return DeepAnalyze.AnalysisResult(description: text, proposedName: nil)
                    } catch { return DeepAnalyze.AnalysisResult(description: "Inference failed: \(error.localizedDescription)", proposedName: nil) }
                }
                return DeepAnalyzeRunner.isAnalysisFailure(result) || result.description.isEmpty ? nil : result.description
            }
            let details = (try? await database.pool.read { db in try Self.evidenceDetails(db, hits.prefix(8).map(\.fileID)) }) ?? [:]
            let evidence = Self.evidencePrompt(request: request.text ?? "", hits: hits, history: history, details: details)
            if let first = await generate(evidence + "\n" + ChatActions.actionToolFormat) {
                answer = first
                // Some models answer with only the <actions> line. Ask again, plainly, so the user gets words.
                let stillActive = active[request.conversationID] == request.requestID
                if stillActive, ChatActions.modelProposals(first).0.isEmpty, let plain = await generate(evidence) {
                    answer = plain
                }
            }
            if !interleaved { await DeepAnalyze.shared.clearCancel() }
        }
        guard active[request.conversationID] == request.requestID else { return }
        let (prose, proposals) = ChatActions.modelProposals(answer)
        let message = prose.isEmpty ? fallback : prose
        let actions = await Self.modelActions(request, proposals: proposals, database: database)
        do {
            try await Self.save(message, role: "assistant", conversation: request.conversationID, database: database)
            guard active[request.conversationID] == request.requestID else { return }
            active.removeValue(forKey: request.conversationID)
            await emit(request, status: "completed", message: message, hits: hits, actions: actions, database: database, sink: sink)
        } catch {
            if active[request.conversationID] == request.requestID { active.removeValue(forKey: request.conversationID) }
            await emit(request, status: "error", message: error.localizedDescription, hits: hits, database: database, sink: sink)
        }
    }

    private func stream(_ request: ChatRequest, chunk: String, hits: [CatalogHit], database: Database, sink: IPCSink) async {
        guard active[request.conversationID] == request.requestID else { return }
        accumulated[request.requestID, default: ""] += chunk
        let now = Date()
        if now.timeIntervalSince(lastEmit[request.requestID] ?? .distantPast) >= 0.15 {
            lastEmit[request.requestID] = now
            let text = accumulated[request.requestID] ?? ""
            let visible = text.range(of: "<actions>").map { String(text[..<$0.lowerBound]) } ?? text
            await emit(request, status: "streaming", message: visible, hits: hits, database: database, sink: sink)
        }
    }

    static func save(_ text: String, role: String, conversation: String, database: Database) async throws {
        try await database.pool.write { db in try db.execute(sql: "INSERT INTO catalog_chat(id,conversation_id,role,text,created_at) VALUES(?,?,?,?,?)", arguments: [UUID().uuidString,conversation,role,text,Date().timeIntervalSince1970]) }
    }

    func emit(_ request: ChatRequest, status: String, message: String, hits: [CatalogHit] = [], actions: [ChatAction] = [], database: Database, sink: IPCSink) async {
        let history = (try? await database.pool.read { db in
            try Row.fetchAll(db, sql: "SELECT * FROM (SELECT rowid,* FROM catalog_chat WHERE conversation_id=? ORDER BY rowid DESC LIMIT 100) ORDER BY rowid", arguments: [request.conversationID]).map { row in
                ChatMessage(id: row["id"], role: row["role"], text: row["text"], createdAt: row["created_at"])
            }
        }) ?? []
        if request.action == "send", status != "error", latestRequest[request.conversationID] != request.requestID { return }
        let validated = actions.filter(ChatActions.valid)
        await sink.emit(.chatResponse(ChatResponse(requestID: request.requestID, conversationID: request.conversationID, status: status, message: message, messages: history, hits: hits, actions: validated)))
        if ["completed", "cancelled", "error"].contains(status), latestRequest[request.conversationID] == request.requestID {
            latestRequest.removeValue(forKey: request.conversationID)
        }
    }
}
