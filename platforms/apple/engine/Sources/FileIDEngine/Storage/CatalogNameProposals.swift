import Foundation
import GRDB
import FileIDShared

/// `proposeNames`: preview smart names in a saved style from stored catalog
/// evidence only. No model call, no file or row change. Lockstep with Rust
/// `commands/catalog_names.rs`.
enum CatalogNameProposals {
    typealias S = SmartNameStyle
    typealias C = SmartNameCompanions
    static let maxFiles = 1000

    static func handle(_ request: CatalogRequest, database: Database) async throws -> CatalogResponse {
        guard let naming = request.naming, let style = S.Style(rawValue: naming.style),
              (S.minLength...S.maxLength).contains(naming.maxLength),
              let ids = request.fileIDs, !ids.isEmpty, ids.count <= maxFiles else { throw CatalogStore.InvalidRequest() }
        let options = S.Options(style: style, maxLength: naming.maxLength, people: naming.includePeople,
                                event: naming.includeEvent, date: naming.includeDate)
        let hits = try await database.pool.read { db in try propose(db, ids: ids, options: options) }
        return CatalogResponse(requestID: request.requestID, status: "ok", hits: hits)
    }

    static func propose(_ db: GRDB.Database, ids: [Int64], options: S.Options) throws -> [CatalogHit] {
        var run = Run(db: db, options: options, personWords: try personWords(db))
        for id in ids where run.seen.insert(id).inserted {
            guard let file = try load(db, id: id) else { continue }
            let members = C.members(file.name, listing: run.folder(file.parent).names)
            if members.isEmpty { run.single(id, file) } else { try run.group(id, file, members) }
        }
        return run.hits
    }

    struct Folder {
        let names: [String]
        let taken: Set<S.Key>
    }

    struct Companion {
        let member: C.Member
        let path: String
        let row: Int64?
    }

    static func leaderFirst(_ leader: Int, _ count: Int) -> [Int] { [leader] + (0..<count).filter { $0 != leader } }

    struct Run {
        let db: GRDB.Database
        let options: S.Options
        let personWords: Set<S.Key>
        var folders: [String: Folder] = [:]
        var claimed: [String: Set<S.Key>] = [:]
        var seen = Set<Int64>()
        var hits: [CatalogHit] = []

        mutating func folder(_ directory: String) -> Folder {
            if let known = folders[directory] { return known }
            let names = (try? FileManager.default.contentsOfDirectory(atPath: directory)) ?? []
            let folder = Folder(names: names, taken: Set(names.map { S.key(S.lower($0)) }))
            folders[directory] = folder
            return folder
        }

        func nameOutcome(_ evidence: S.Evidence) -> S.Outcome {
            var evidence = evidence
            evidence.personWords = personWords
            return S.propose(evidence, options)
        }

        mutating func names(_ parent: String, stem: String, members: [(suffix: String, own: S.Key)]) -> [String]? {
            let taken = folder(parent).taken
            guard let names = C.sharedNames(stem: stem, members: members, taken: taken, claimed: claimed[parent] ?? []) else { return nil }
            if zip(names, members).contains(where: { S.key(S.lower($0)) != $1.own }) {
                claimed[parent, default: []].formUnion(names.map { S.key(S.lower($0)) })
            }
            return names
        }

        mutating func push(_ id: Int64, _ path: String, _ text: String, _ reason: String) {
            hits.append(CatalogHit(fileID: id, path: path, kind: "proposedName", text: text, evidenceID: reason))
        }

        mutating func single(_ id: Int64, _ file: Loaded) {
            let outcome = nameOutcome(file.evidence)
            var text = "", reason = outcome.reason
            if let stem = outcome.stem {
                let own = S.key(S.lower(file.name))
                switch names(file.parent, stem: stem, members: [(file.ext.isEmpty ? "" : "." + file.ext, own)]) {
                case let names? where S.key(S.lower(names[0])) == own: reason = "unchanged"
                case let names?: text = names[0]
                case nil: reason = "collision"
                }
            }
            push(id, file.path, text, reason)
        }

        mutating func group(_ id: Int64, _ file: Loaded, _ members: [C.Member]) throws {
            var resolved: [Companion] = []
            for member in members {
                let path = file.parent == "/" ? "/" + member.name : file.parent + "/" + member.name
                let row = S.key(member.name) == S.key(file.name)
                    ? id : try Int64.fetchOne(db, sql: "SELECT id FROM files WHERE path_text=?", arguments: [path])
                if row != nil || (member.role != .sidecar && isRegularFile(path)) { resolved.append(Companion(member: member, path: path, row: row)) }
            }
            guard C.isGroup(resolved.map(\.member)) else { single(id, file); return }
            seen.formUnion(resolved.compactMap(\.row))
            let known = resolved.compactMap { c in c.row.map { (c, $0) } }
            if known.count < resolved.count {
                let leader = known.firstIndex { $0.0.member.role != .sidecar } ?? 0
                let reason = C.groupReason(C.unscanned, leader: known[leader].1)
                for index in leaderFirst(leader, known.count) { push(known[index].1, known[index].0.path, "", reason) }
                return
            }
            var outcomes: [S.Outcome?] = []
            for (c, row) in known {
                let loaded = row == id ? file : try load(db, id: row)
                outcomes.append(c.member.role != .sidecar ? loaded.map { nameOutcome($0.evidence) } : nil)
            }
            var best: (index: Int, rank: Int)?
            for (index, outcome) in outcomes.enumerated() {
                guard let outcome else { continue }
                let rank = C.sourceRank(outcome.source, style: options.style)
                if best.map({ rank < $0.rank }) ?? true { best = (index, rank) }
            }
            let leader = best?.index ?? known.firstIndex { $0.0.member.role != .sidecar } ?? 0
            let order = leaderFirst(leader, known.count)
            let outcome = outcomes[leader] ?? S.Outcome(stem: nil, reason: "noEvidence", source: "noEvidence")
            var texts = Array(repeating: "", count: order.count)
            var reason = outcome.reason
            if let stem = outcome.stem {
                let specs = order.map { (suffix: known[$0].0.member.suffix, own: S.key(S.lower(known[$0].0.member.name))) }
                switch names(file.parent, stem: stem, members: specs) {
                case let names? where zip(names, specs).allSatisfy({ S.key(S.lower($0)) == $1.own }): reason = "unchanged"
                case let names?: texts = names
                case nil: reason = "collision"
                }
            }
            let tagged = C.groupReason(reason, leader: known[leader].1)
            for (text, index) in zip(texts, order) { push(known[index].1, known[index].0.path, text, tagged) }
        }

        func isRegularFile(_ path: String) -> Bool {
            var info = stat()
            return stat(path, &info) == 0 && (info.st_mode & S_IFMT) == S_IFREG
        }
    }

    static func personWords(_ db: GRDB.Database) throws -> Set<S.Key> {
        var words = Set<S.Key>()
        for row in try Row.fetchAll(db, sql: "SELECT title,first_name,middle_name,last_name,suffix,name FROM persons") {
            for index in 0..<6 { if let part: String = row[index] { S.addPersonWords(&words, part) } }
        }
        return words
    }

    struct Loaded {
        let path: String
        let parent: String
        let name: String
        let ext: String
        var evidence: S.Evidence
    }

    static func load(_ db: GRDB.Database, id: Int64) throws -> Loaded? {
        guard let row = try Row.fetchOne(db, sql: "SELECT path_text,size_bytes,modified_at,created_at,COALESCE(failed,0) AS failed,vlm_description,vlm_proposed_name FROM files WHERE id=?", arguments: [id]) else { return nil }
        let path: String = row["path_text"]
        let size: Int64 = row["size_bytes"]
        let modified: Double? = row["modified_at"]
        let created: Double? = row["created_at"]
        let failed: Int64 = row["failed"]
        let revision = "\(size):\(modified.map { String($0.bitPattern) } ?? "unknown")"
        let slash = path.unicodeScalars.lastIndex(of: "/")
        let name = slash.map { String(path.unicodeScalars[path.unicodeScalars.index(after: $0)...]) } ?? path
        let parent = slash.map { $0 == path.unicodeScalars.startIndex ? "/" : String(path.unicodeScalars[..<$0]) } ?? ""
        let (stem, ext) = S.splitName(name)
        let chapters = try String.fetchAll(db, sql: "SELECT title FROM catalog_chapters WHERE file_id=? AND user_edited=1 AND stale=0 AND source_revision=? ORDER BY id", arguments: [id, revision])
        let events = try String.fetchAll(db, sql: "SELECT e.title FROM catalog_events e JOIN catalog_event_files ef ON ef.event_id=e.id WHERE ef.file_id=? AND e.user_edited=1 ORDER BY e.id", arguments: [id])
        let tags = try String.fetchAll(db, sql: "SELECT tag FROM tags WHERE file_id=? ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, tag", arguments: [id])
            .filter { !S.key(S.lower($0)).starts(with: "year_".unicodeScalars) }
        let evidence = S.Evidence(originalStem: stem, chapter: S.singleLabel(chapters), storedName: row["vlm_proposed_name"],
                                  description: row["vlm_description"], tags: tags,
                                  people: try confirmedPeople(db, id: id, revision: revision, failed: failed != 0),
                                  event: S.singleLabel(events), date: created.flatMap(captureDate))
        return Loaded(path: path, parent: parent, name: name, ext: ext, evidence: evidence)
    }

    static func confirmedPeople(_ db: GRDB.Database, id: Int64, revision: String, failed: Bool) throws -> [String] {
        var tokens: [String] = []
        func accept(_ row: Row) {
            let title: String? = row["title"], first: String? = row["first_name"], legacy: String? = row["name"]
            if S.isUserNamed(S.personDisplay(title: title, first: first, legacy: legacy)),
               let token = S.personToken(first: first, legacy: legacy) { tokens.append(token) }
        }
        if !failed {
            let faces = try Row.fetchAll(db, sql: """
                SELECT p.title,p.first_name,p.name,fp.arcface_embedding,fp.embedding_model AS model,fp.processing_version AS processing,
                       fp.source_revision AS source,rep.embedding_model AS rep_model,rep.processing_version AS rep_processing
                FROM face_prints fp JOIN persons p ON p.id=fp.person_id JOIN face_prints rep ON rep.id=p.representative_face_id
                WHERE fp.file_id=? AND COALESCE(fp.excluded,0)=0 AND COALESCE(rep.excluded,0)=0 AND IFNULL(p.is_unknown,0)=0 ORDER BY fp.id
                """, arguments: [id])
            for row in faces {
                let model: String? = row["model"], processing: String? = row["processing"]
                let repModel: String? = row["rep_model"], repProcessing: String? = row["rep_processing"]
                let source: String? = row["source"]
                let embedding: Data? = row["arcface_embedding"]
                let compatible = !(model ?? "").isEmpty && !(processing ?? "").isEmpty && source == revision
                    && repModel == model && repProcessing == processing
                    && embedding.map(FaceCacheProvenance.normalizedEmbedding) == true
                if compatible { accept(row) }
            }
        }
        for row in try Row.fetchAll(db, sql: """
            SELECT p.title,p.first_name,p.name FROM catalog_observations o JOIN persons p ON p.id=o.person_id
            WHERE o.file_id=? AND o.user_edited=1 AND o.stale=0 AND o.source_revision=? AND IFNULL(p.is_unknown,0)=0 ORDER BY o.id
            """, arguments: [id, revision]) { accept(row) }
        var unique: [String] = []
        for token in tokens.sorted(by: { $0.unicodeScalars.lexicographicallyPrecedes($1.unicodeScalars) })
        where unique.last.map(S.key) != S.key(token) { unique.append(token) }
        return unique
    }

    static func captureDate(_ created: Double) -> String? {
        guard created.isFinite, created > 0 else { return nil }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .current
        let parts = calendar.dateComponents([.year, .month, .day], from: Date(timeIntervalSince1970: created.rounded(.down)))
        guard let year = parts.year, let month = parts.month, let day = parts.day, (1900..<2100).contains(year) else { return nil }
        return String(format: "%04d-%02d-%02d", year, month, day)
    }
}
