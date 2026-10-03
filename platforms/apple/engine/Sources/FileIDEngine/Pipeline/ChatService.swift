import Foundation
import GRDB
import FileIDShared

actor ChatService {
    static let shared = ChatService()
    private var active: [String: String] = [:]
    private var running: String?
    private var accumulated: [String: String] = [:]
    private var lastEmit: [String: Date] = [:]
    private var latestRequest: [String: String] = [:]

    func handle(_ request: ChatRequest, database: Database, sink: IPCSink) async {
        do {
            guard !request.requestID.isEmpty, request.requestID.count <= 200,
                  !request.conversationID.isEmpty, request.conversationID.count <= 200 else { throw CatalogStore.InvalidRequest() }
            switch request.action {
            case "history":
                await emit(request, status: active[request.conversationID] == nil ? "completed" : "queued", message: active[request.conversationID] == nil ? "Local conversation history." : "A local model summary is still queued or running.", database: database, sink: sink)
            case "clear", "cancel":
                latestRequest[request.conversationID] = request.requestID
                if let id = active.removeValue(forKey: request.conversationID) {
                    await JobQueue.shared.cancelPending(id: "chat-" + id)
                    if running == id { await DeepAnalyze.shared.requestCancel() }
                    accumulated.removeValue(forKey: id); lastEmit.removeValue(forKey: id)
                }
                if request.action == "clear" {
                    try await database.pool.write { db in try db.execute(sql: "DELETE FROM catalog_chat WHERE conversation_id=?", arguments: [request.conversationID]) }
                }
                await emit(request, status: request.action == "clear" ? "completed" : "cancelled", message: request.action == "clear" ? "Conversation deleted." : "Model response stopped.", database: database, sink: sink)
            case "send":
                guard active[request.conversationID] == nil, active.count < 8, let text = request.text,
                      !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, text.count <= 2000 else { throw CatalogStore.InvalidRequest() }
                active[request.conversationID] = request.requestID
                latestRequest[request.conversationID] = request.requestID
                let plan = try await database.pool.write { db in
                    let knownPeople = try Self.knownPeople(db)
                    let previous = try String.fetchAll(db, sql: "SELECT text FROM (SELECT rowid,text FROM catalog_chat WHERE conversation_id=? AND role='user' ORDER BY rowid DESC LIMIT 20) ORDER BY rowid", arguments: [request.conversationID])
                        .reduce(nil as ChatSearchPlan?) { ChatSearchPlan.resolve($1, previous: $0, knownPeople: knownPeople) }
                    let plan = ChatSearchPlan.resolve(text, previous: previous, knownPeople: knownPeople)
                    try db.execute(sql: "INSERT INTO catalog_chat(id,conversation_id,role,text,created_at) VALUES(?,?,'user',?,?)", arguments: [UUID().uuidString, request.conversationID, text, Date().timeIntervalSince1970])
                    return plan
                }
                let hits = try await database.pool.read { db in try CatalogStore.search(db, query: plan.query, kinds: plan.kinds, personIDs: plan.personIDs) }
                guard active[request.conversationID] == request.requestID else { return }
                let scope = plan.query.isEmpty ? "all catalog files" : "“\(plan.query)”"
                let filter = plan.kinds.isEmpty ? "" : " (\(plan.kinds.joined(separator: ", ")))"
                let people = plan.personNames.isEmpty ? "" : " (people: \(plan.personNames.joined(separator: ", ")))"
                let explanation = plan.query.isEmpty && plan.kinds.isEmpty && plan.personIDs.isEmpty ? "Add a subject or a media type such as videos or photos. No search was run." : hits.isEmpty ? "No keyword matches for \(scope)\(filter)\(people). Try names or a few descriptive terms. Unanalyzed files may still contain the requested event." : "Found \(hits.count) file or evidence matches for \(scope)\(filter)\(people). Sampled-frame descriptions remain unverified."
                await emit(request, status: "retrieving", message: explanation, hits: hits, database: database, sink: sink)
                guard active[request.conversationID] == request.requestID else { return }
                if request.useModel == true, !hits.isEmpty, case .ready(let model) = await DeepAnalyze.shared.loadState {
                    active[request.conversationID] = request.requestID
                    await emit(request, status: "queued", message: "Results are ready. The loaded local model will summarize their evidence when its current work finishes.", hits: hits, database: database, sink: sink)
                    guard active[request.conversationID] == request.requestID else { return }
                    await JobQueue.shared.enqueue(.init(id: "chat-" + request.requestID, category: .deepAnalyze, title: "Chat evidence summary", etaSeconds: nil, priority: .interactive) {
                        await ChatService.shared.summarize(request, hits: hits, model: model, fallback: explanation, database: database, sink: sink)
                    })
                } else {
                    let message = explanation + (request.useModel == true ? " No model was loaded or no evidence was available; no download was started." : "")
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

    static func query(_ text: String) -> String {
        ChatSearchPlan.resolve(text).query
    }

    private static func knownPeople(_ db: GRDB.Database) throws -> [ChatSearchPlan.KnownPerson] {
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

    private func summarize(_ request: ChatRequest, hits: [CatalogHit], model: AIModelKind, fallback: String, database: Database, sink: IPCSink) async {
        guard active[request.conversationID] == request.requestID else { return }
        running = request.requestID
        defer { running = nil; accumulated.removeValue(forKey: request.requestID); lastEmit.removeValue(forKey: request.requestID) }
        var answer = fallback
        if case .ready(let current) = await DeepAnalyze.shared.loadState, current == model {
            await DeepAnalyze.shared.clearCancel()
            let facts = hits.prefix(8).enumerated().map { index, hit in
                ["number": String(index + 1), "file": URL(fileURLWithPath: hit.path).lastPathComponent, "kind": hit.kind, "evidence": String(hit.text.prefix(600)), "seconds": hit.startSeconds.map { String($0) } ?? "", "page": hit.page.map { String($0) } ?? ""]
            }
            if let data = try? JSONEncoder().encode(facts) {
                let prompt = "Request: \(request.text ?? "")\nCatalog evidence JSON: " + String(decoding: data, as: UTF8.self)
                let result = await DeepAnalyze.shared.runCancellableAnalysis {
                    do {
                        let text = try await DeepAnalyze.shared.answerCatalog(prompt: prompt) { chunk in
                            await ChatService.shared.stream(request, chunk: chunk, hits: hits, database: database, sink: sink)
                        }
                        return DeepAnalyze.AnalysisResult(description: text, proposedName: nil)
                    } catch { return DeepAnalyze.AnalysisResult(description: "Inference failed: \(error.localizedDescription)", proposedName: nil) }
                }
                if !DeepAnalyzeRunner.isAnalysisFailure(result), !result.description.isEmpty { answer = result.description }
            }
            await DeepAnalyze.shared.clearCancel()
        }
        guard active[request.conversationID] == request.requestID else { return }
        do {
            try await Self.save(answer, role: "assistant", conversation: request.conversationID, database: database)
            guard active[request.conversationID] == request.requestID else { return }
            active.removeValue(forKey: request.conversationID)
            await emit(request, status: "completed", message: answer, hits: hits, database: database, sink: sink)
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
            await emit(request, status: "streaming", message: accumulated[request.requestID] ?? "", hits: hits, database: database, sink: sink)
        }
    }

    private static func save(_ text: String, role: String, conversation: String, database: Database) async throws {
        try await database.pool.write { db in try db.execute(sql: "INSERT INTO catalog_chat(id,conversation_id,role,text,created_at) VALUES(?,?,?,?,?)", arguments: [UUID().uuidString,conversation,role,text,Date().timeIntervalSince1970]) }
    }

    private func emit(_ request: ChatRequest, status: String, message: String, hits: [CatalogHit] = [], database: Database, sink: IPCSink) async {
        let history = (try? await database.pool.read { db in
            try Row.fetchAll(db, sql: "SELECT * FROM (SELECT rowid,* FROM catalog_chat WHERE conversation_id=? ORDER BY rowid DESC LIMIT 100) ORDER BY rowid", arguments: [request.conversationID]).map { row in
                ChatMessage(id: row["id"], role: row["role"], text: row["text"], createdAt: row["created_at"])
            }
        }) ?? []
        if request.action == "send", status != "error", latestRequest[request.conversationID] != request.requestID { return }
        await sink.emit(.chatResponse(ChatResponse(requestID: request.requestID, conversationID: request.conversationID, status: status, message: message, messages: history, hits: hits)))
        if ["completed", "cancelled", "error"].contains(status), latestRequest[request.conversationID] == request.requestID {
            latestRequest.removeValue(forKey: request.conversationID)
        }
    }
}
