import Foundation
import GRDB
import FileIDShared

enum FaceClusterPersistence {
    struct Cluster: Sendable {
        let personID: Int64?
        let faceIDs: [Int64]
        let representative: Int64
        let centroid: Data
        let radius: Double
    }

    enum Failure: Error { case invalidPlan; case personLimit }

    static func apply(_ db: GRDB.Database, clusters: [Cluster], pool: Set<Int64>, preserved: Set<Int64>, now: Double, maximumPersons: Int = FaceClustering.maxPersons) throws -> Int {
        guard db.isInsideTransaction, now.isFinite else { throw Failure.invalidPlan }
        var seen = Set<Int64>()
        var reused = Set<Int64>()
        for cluster in clusters {
            guard !cluster.faceIDs.isEmpty, cluster.faceIDs.contains(cluster.representative),
                  cluster.centroid.count == 512, ArcFaceService.blobToEmbedding(cluster.centroid).allSatisfy(\.isFinite), cluster.radius.isFinite,
                  cluster.faceIDs.allSatisfy({ $0 > 0 && pool.contains($0) && seen.insert($0).inserted }) else { throw Failure.invalidPlan }
            if let person = cluster.personID {
                guard person > 0, !preserved.contains(person), reused.insert(person).inserted,
                      try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM persons WHERE id=? AND COALESCE(is_unknown,0)=0", arguments: [person]) == 1 else { throw Failure.invalidPlan }
            }
        }
        let existing = try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM persons") ?? 0
        guard existing + clusters.filter({ $0.personID == nil }).count <= maximumPersons else { throw Failure.personLimit }
        let selected = try json(pool)
        let protected = try json(preserved)
        let outputFaces = try json(seen)
        guard try Int.fetchOne(db, sql: """
            SELECT COUNT(*) FROM face_prints WHERE id IN (SELECT value FROM json_each(?)) AND excluded=0
              AND (person_id IS NULL OR person_id NOT IN (SELECT value FROM json_each(?)))
            """, arguments: [outputFaces,protected]) == seen.count else { throw Failure.invalidPlan }
        try db.execute(sql: """
            UPDATE face_prints SET person_id=NULL WHERE excluded=0 AND id IN (SELECT value FROM json_each(?))
              AND (person_id IS NULL OR person_id NOT IN (SELECT value FROM json_each(?)))
            """, arguments: [selected,protected])
        for cluster in clusters {
            let personID: Int64
            if let existingID = cluster.personID {
                try db.execute(sql: """
                    UPDATE persons SET representative_face_id=?,centroid=?,anchor_radius=?,last_clustered_at=? WHERE id=?
                    """, arguments: [cluster.representative,cluster.centroid,cluster.radius,now,existingID])
                personID = existingID
            } else {
                try db.execute(sql: """
                    INSERT INTO persons(representative_face_id,file_count,created_at,centroid,anchor_radius,last_clustered_at)
                    VALUES(?,0,?,?,?,?)
                    """, arguments: [cluster.representative,now,cluster.centroid,cluster.radius,now])
                personID = db.lastInsertedRowID
            }
            try db.execute(sql: "UPDATE face_prints SET person_id=? WHERE id IN (SELECT value FROM json_each(?))", arguments: [personID,try json(Set(cluster.faceIDs))])
        }
        try db.execute(sql: """
            UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=persons.id)
            """)
        try FaceClustering.repairDanglingRepresentativeFaces(db)
        return try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM persons") ?? 0
    }

    private static func json(_ ids: Set<Int64>) throws -> String {
        String(decoding: try JSONEncoder().encode(ids.sorted()), as: UTF8.self)
    }
}
