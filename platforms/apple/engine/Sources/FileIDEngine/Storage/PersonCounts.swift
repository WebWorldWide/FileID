import GRDB

/// `persons.file_count`: the number of distinct files holding a counted face of
/// the person. A face counts unless `excluded` is set. The Rust engine runs the
/// same expression (`db/person_counts.rs`).
enum PersonCounts {
    private static let count = """
        (SELECT COUNT(DISTINCT file_id) FROM face_prints \
        WHERE person_id = persons.id AND COALESCE(excluded,0)=0)
        """

    /// Corrects every stored count that differs from the live one, including a
    /// person left with no counted face. Returns the number of rows changed.
    @discardableResult
    static func recountStale(_ db: GRDB.Database) throws -> Int {
        try db.execute(sql: "UPDATE persons SET file_count = \(count) WHERE file_count IS NOT \(count)")
        return db.changesCount
    }

    /// The persons with a face on any of `fileIDs`: read these before the files go.
    static func persons(ofFiles fileIDs: [Int64], _ db: GRDB.Database) throws -> Set<Int64> {
        var out = Set<Int64>()
        for id in fileIDs {
            out.formUnion(try Int64.fetchAll(
                db, sql: "SELECT DISTINCT person_id FROM face_prints WHERE file_id = ? AND person_id IS NOT NULL",
                arguments: [id]))
        }
        return out
    }
}
