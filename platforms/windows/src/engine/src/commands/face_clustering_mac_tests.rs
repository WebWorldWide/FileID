use super::*;
use crate::db::test_support::memory_db;
use std::path::PathBuf;

const MODEL: &str = "sface128-sha256:test";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    let path = std::env::temp_dir().join(format!("fileid-face-mac-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path.canonicalize().unwrap())
}

fn space() -> Space {
    Space { model: MODEL.into(), processing: crate::commands::face_refresh::MAC_PROCESSING.into() }
}

/// Unit vector near axis `identity`, nudged by `jitter` along axis 100 + `face`.
fn vector(identity: usize, face: usize, jitter: f32) -> Vec<u8> {
    let mut values = vec![0.0f32; DIM];
    values[identity] = 1.0;
    values[100 + face % 28] = jitter;
    let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    encode(&values.iter().map(|v| v / norm).collect::<Vec<_>>())
}

struct Catalog {
    conn: Connection,
    dir: TempDir,
}

impl Catalog {
    fn new() -> Self {
        let conn = memory_db();
        Self { conn, dir: tempdir() }
    }

    /// A photo on disk with one face, stamped as the Swift engine stamps it.
    fn face(&self, id: i64, identity: usize, quality: f64, person: Option<i64>) {
        let path = self.dir.0.join(format!("{id}.jpg"));
        std::fs::write(&path, format!("photo {id}")).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let (size, modified) = (metadata.len() as i64, foundation_mtime(&metadata).unwrap());
        let revision = source_revision(size, modified);
        self.conn
            .execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(?1,?2,?1,?3,?4,0,'image','jpg')",
                params![id, path.to_string_lossy(), size, modified],
            )
            .unwrap();
        self.conn
            .execute("INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?1,?2,'file-stat-v1',0)", params![id, revision])
            .unwrap();
        self.conn
            .execute(
                "INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,face_quality,embedding_model,processing_version,source_revision) \
                 VALUES(?1,?1,?2,X'00',?3,?4,?5,?6,?7,?8)",
                params![id, person, format!("0.1,0.1,0.2,0.{id}"), vector(identity, id as usize, 0.05), quality, MODEL, space().processing, revision],
            )
            .unwrap();
    }

    fn person(&self, id: i64, name: Option<&str>, unknown: bool) {
        self.conn
            .execute("INSERT INTO persons(id,name,is_unknown,created_at,file_count) VALUES(?1,?2,?3,0,0)", params![id, name, i64::from(unknown)])
            .unwrap();
    }

    fn assignments(&self) -> Vec<(i64, Option<i64>)> {
        let mut statement = self.conn.prepare("SELECT id, person_id FROM face_prints ORDER BY id").unwrap();
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    fn run(self) -> (Self, Outcome) {
        let database = Arc::new(Mutex::new(self.conn));
        let outcome = super::run(&database, &space());
        let conn = Arc::try_unwrap(database).ok().unwrap().into_inner();
        (Self { conn, dir: self.dir }, outcome)
    }
}

#[test]
fn foundation_time_matches_the_swift_engine() {
    for (seconds, nanos, stored) in [
        (1_794_315_047_i64, 172_200_036_i64, 1_794_315_047.172_200_2_f64),
        (1_794_315_049, 370_255_120, 1_794_315_049.370_255),
        (1_794_315_049, 438_319_825, 1_794_315_049.438_319_7),
    ] {
        assert_eq!(foundation_time(seconds, nanos).to_bits(), stored.to_bits());
    }
}

#[test]
fn a_swift_catalog_keeps_person_ids_names_and_unknowns() {
    let catalog = Catalog::new();
    catalog.person(10, Some("Alex"), false);
    catalog.person(20, None, false);
    catalog.person(30, None, true);
    for face in 1..=4 {
        catalog.face(face, 0, 0.6, Some(10));
    }
    for face in 5..=8 {
        catalog.face(face, 1, 0.6, Some(20));
    }
    for face in 9..=11 {
        catalog.face(face, 2, 0.6, Some(30));
    }
    catalog
        .conn
        .execute("INSERT INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b) VALUES(10,20,0,1,'user',0,1,5)", [])
        .unwrap();
    let before = catalog.assignments();
    let (catalog, outcome) = catalog.run();
    assert!(outcome.error.is_none());
    assert_eq!(catalog.assignments(), before);
    let named: String = catalog.conn.query_row("SELECT name FROM persons WHERE id=10", [], |row| row.get(0)).unwrap();
    assert_eq!(named, "Alex");
    let unknown: i64 = catalog.conn.query_row("SELECT is_unknown FROM persons WHERE id=30", [], |row| row.get(0)).unwrap();
    assert_eq!(unknown, 1);
    let verdicts: i64 = catalog.conn.query_row("SELECT COUNT(*) FROM face_verifications", [], |row| row.get(0)).unwrap();
    assert_eq!(verdicts, 1);
    assert_eq!(outcome.result.deferred_faces, Some(0));
    assert_eq!(outcome.result.face_count, 8);
    let (again, _) = catalog.run();
    assert_eq!(again.assignments(), before);
}

#[test]
fn fresh_faces_form_people_and_small_low_quality_groups_stay_unassigned() {
    let catalog = Catalog::new();
    for face in 1..=4 {
        catalog.face(face, 0, 0.6, None);
    }
    catalog.face(5, 1, 0.1, None);
    let (catalog, outcome) = catalog.run();
    assert!(outcome.error.is_none());
    let assignments = catalog.assignments();
    let first = assignments[0].1.unwrap();
    assert!(assignments[..4].iter().all(|(_, person)| *person == Some(first)));
    assert_eq!(assignments[4].1, None);
    assert_eq!(outcome.result.unmatched_faces, 1);
}

#[test]
fn a_changed_photo_is_deferred_not_clustered() {
    let catalog = Catalog::new();
    for face in 1..=4 {
        catalog.face(face, 0, 0.6, None);
    }
    std::fs::write(catalog.dir.0.join("4.jpg"), "edited since the scan").unwrap();
    let (catalog, outcome) = catalog.run();
    assert_eq!(outcome.result.deferred_faces, Some(1));
    assert_eq!(catalog.assignments()[3].1, None);
}

#[test]
fn stable_ids_go_to_the_largest_overlap() {
    let priors = vec![(7, vec![1, 2, 3]), (8, vec![4, 5])];
    let clusters = vec![(vec![1, 2, 4], 1), (vec![3, 5], 5)];
    let ids = stable_ids(&priors, &clusters, &[None, None], &HashSet::new()).unwrap();
    assert_eq!(ids, vec![Some(7), Some(8)]);
    assert!(stable_ids(&priors, &clusters, &[Some(9), None], &HashSet::new()).is_err());
}

#[test]
fn the_vantage_point_join_finds_every_pair_the_sweep_finds() {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
    };
    let vectors: Vec<Vec<f32>> = (0..760)
        .map(|index| {
            let mut values: Vec<f32> = (0..8).map(|_| next()).collect();
            values[index % 8] += 1.5;
            let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
            values.iter().map(|v| v / norm).collect()
        })
        .collect();
    let small: Vec<bool> = (0..vectors.len()).map(|index| index % 3 == 0).collect();
    let tree = Join { vectors: &vectors, small: &small, tight: 0.9, loose: 0.8, evaluations: 0, exceeded: false, found: Vec::new() }.edges().unwrap();
    let mut sweep = Vec::new();
    for i in 0..vectors.len() {
        for j in i + 1..vectors.len() {
            let cosine = dot(&vectors[i], &vectors[j]);
            if cosine >= 0.9 || (cosine >= 0.8 && (small[i] || small[j])) {
                sweep.push((cosine, i, j));
            }
        }
    }
    sweep.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    assert!(vectors.len() * (vectors.len() - 1) / 2 > JOIN_DIRECT_PAIRS);
    assert!(!tree.is_empty());
    assert_eq!(tree, sweep);
}

#[test]
fn held_faces_are_only_ever_uncertain() {
    let scored = vec![Scored { person: 1, score: 0.9, unknown: false }, Scored { person: 2, score: 0.1, unknown: false }];
    assert_eq!(decide(&scored, &HashSet::new(), false).outcome, "suggested");
    assert_eq!(decide(&scored, &HashSet::new(), true).outcome, "uncertain");
    let unknown_first = vec![Scored { person: 3, score: 0.9, unknown: true }, Scored { person: 1, score: 0.8, unknown: false }];
    assert_eq!(decide(&unknown_first, &HashSet::new(), false).outcome, "unmatched");
}
