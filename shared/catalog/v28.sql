CREATE TABLE take_quality (file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,source_revision TEXT NOT NULL,version TEXT NOT NULL,signals_json TEXT NOT NULL,computed_at REAL NOT NULL);
CREATE TABLE take_group_decisions (group_key TEXT PRIMARY KEY,decision TEXT NOT NULL CHECK(decision IN ('keepAll','skip')),file_ids_json TEXT NOT NULL,decided_at REAL NOT NULL);
