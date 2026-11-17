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
        let (size, modified) = (metadata.len() as i64, catalog_mtime(&metadata).unwrap());
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

/// Two people whose faces are the same identity (axis 0), four faces each, so the only thing that can keep
/// them apart is what the user has said about them.
fn twin_people(first: (Option<&str>, bool), second: (Option<&str>, bool), verdict: Option<bool>) -> (Catalog, usize) {
    let catalog = Catalog::new();
    catalog.person(10, first.0, first.1);
    catalog.person(20, second.0, second.1);
    for face in 1..=4 {
        catalog.face(face, 0, 0.6, Some(10));
    }
    for face in 5..=8 {
        catalog.face(face, 0, 0.6, Some(20));
    }
    if let Some(same) = verdict {
        catalog
            .conn
            .execute(
                "INSERT INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at,face_a,face_b) VALUES(10,20,?1,1,'user',0,1,5)",
                [i64::from(same)],
            )
            .unwrap();
    }
    let mut conn = catalog.conn;
    let absorbed = auto_merge(&mut conn, &space()).unwrap();
    (Catalog { conn, dir: catalog.dir }, absorbed)
}

fn owners(catalog: &Catalog) -> Vec<Option<i64>> {
    catalog.assignments().into_iter().map(|(_, person)| person).collect()
}

#[test]
fn auto_merge_joins_two_unnamed_people_who_are_the_same_identity() {
    let (catalog, absorbed) = twin_people((None, false), (None, false), None);
    assert_eq!(absorbed, 1);
    let owners = owners(&catalog);
    assert!(owners.iter().all(|owner| *owner == owners[0] && owner.is_some()), "{owners:?}");
}

#[test]
fn auto_merge_folds_an_unnamed_person_into_the_named_one_and_never_the_other_way() {
    let (catalog, absorbed) = twin_people((None, false), (Some("Alex"), false), None);
    assert_eq!(absorbed, 1);
    assert!(owners(&catalog).iter().all(|owner| *owner == Some(20)), "the named person keeps every face");
    let name: String = catalog.conn.query_row("SELECT name FROM persons WHERE id=20", [], |row| row.get(0)).unwrap();
    assert_eq!(name, "Alex");
}

#[test]
fn auto_merge_never_joins_two_named_people() {
    let (catalog, absorbed) = twin_people((Some("Alex"), false), (Some("Sam"), false), None);
    assert_eq!(absorbed, 0);
    assert_eq!(owners(&catalog), vec![Some(10), Some(10), Some(10), Some(10), Some(20), Some(20), Some(20), Some(20)]);
}

#[test]
fn auto_merge_never_joins_a_person_marked_unknown() {
    let (catalog, absorbed) = twin_people((None, false), (None, true), None);
    assert_eq!(absorbed, 0);
    assert_eq!(owners(&catalog), vec![Some(10), Some(10), Some(10), Some(10), Some(20), Some(20), Some(20), Some(20)]);
}

#[test]
fn auto_merge_never_joins_two_people_the_user_said_are_different() {
    let (catalog, absorbed) = twin_people((None, false), (None, false), Some(false));
    assert_eq!(absorbed, 0);
    assert_eq!(owners(&catalog), vec![Some(10), Some(10), Some(10), Some(10), Some(20), Some(20), Some(20), Some(20)]);
    let verdicts: i64 = catalog.conn.query_row("SELECT COUNT(*) FROM face_verifications", [], |row| row.get(0)).unwrap();
    assert_eq!(verdicts, 1, "the verdict is kept");
}

#[test]
fn auto_merge_does_not_join_people_who_share_a_photo() {
    let catalog = Catalog::new();
    catalog.person(10, None, false);
    catalog.person(20, None, false);
    for face in 1..=4 {
        catalog.face(face, 0, 0.6, Some(10));
    }
    for face in 5..=8 {
        catalog.face(face, 0, 0.6, Some(20));
    }
    // Face 5 is a second face in photo 1: two people in one photo are two people.
    catalog
        .conn
        .execute("UPDATE face_prints SET file_id=1, source_revision=(SELECT revision FROM catalog_revisions WHERE file_id=1) WHERE id=5", [])
        .unwrap();
    let mut conn = catalog.conn;
    let absorbed = auto_merge(&mut conn, &space()).unwrap();
    assert_eq!(absorbed, 0);
    let people: Vec<Option<i64>> = Catalog { conn, dir: catalog.dir }.assignments().into_iter().map(|(_, person)| person).collect();
    assert_eq!(people, vec![Some(10), Some(10), Some(10), Some(10), Some(20), Some(20), Some(20), Some(20)]);
}

fn many_clusters(count: usize, size_of: impl Fn(usize) -> usize) -> BTreeMap<usize, Vec<usize>> {
    (0..count).map(|id| (id, (0..size_of(id)).collect())).collect()
}

#[test]
fn the_person_cap_keeps_every_protected_person_and_drops_the_smallest_others() {
    let clusters = many_clusters(MAX_PERSONS + 3, |id| if id < 3 { 1 } else { 5 + id % 7 });
    let protected: HashSet<usize> = (0..3).collect();
    let (kept, truncated) = cap(clusters.clone(), &protected, 0).unwrap();
    assert_eq!(kept.len(), MAX_PERSONS);
    assert!((0..3).all(|id| kept.contains_key(&id)), "protected people survive even though they are the smallest");
    assert_eq!(truncated, clusters.values().map(Vec::len).sum::<usize>() - kept.values().map(Vec::len).sum::<usize>());
    let dropped_largest = clusters.iter().filter(|(id, _)| !kept.contains_key(id)).map(|(_, m)| m.len()).max().unwrap();
    let kept_smallest = kept.iter().filter(|(id, _)| **id >= 3).map(|(_, m)| m.len()).min().unwrap();
    assert!(dropped_largest <= kept_smallest, "only the smallest unprotected clusters are dropped");
}

#[test]
fn people_already_kept_use_up_the_person_cap_and_an_overfull_protected_set_refuses() {
    let clusters = many_clusters(10, |_| 4);
    let (kept, truncated) = cap(clusters.clone(), &HashSet::new(), MAX_PERSONS - 6).unwrap();
    assert_eq!((kept.len(), truncated), (6, 16), "only the six free slots are filled");
    let all: HashSet<usize> = clusters.keys().copied().collect();
    assert!(cap(clusters.clone(), &all, MAX_PERSONS - 6).is_err(), "ten protected people do not fit in six slots");
    assert!(cap(clusters.clone(), &HashSet::new(), MAX_PERSONS + 1).is_err(), "preserved people over the cap refuse");
    let (kept, truncated) = cap(clusters, &HashSet::new(), MAX_PERSONS - 10).unwrap();
    assert_eq!((kept.len(), truncated), (10, 0), "a run that fits loses nothing");
}

fn person_ids(catalog: &Catalog) -> Vec<i64> {
    let mut statement = catalog.conn.prepare("SELECT id FROM persons ORDER BY id").unwrap();
    statement.query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

#[test]
fn mixed_legacy_stale_and_malformed_face_caches_are_deferred_and_keep_every_identity() {
    for fault in ["model", "processing", "legacy", "revision", "nan"] {
        let catalog = Catalog::new();
        catalog.person(10, Some("Confirmed"), false);
        catalog.person(20, None, false);
        catalog.face(1, 0, 0.6, Some(10));
        catalog.face(2, 0, 0.6, Some(20));
        let fault_sql = match fault {
            "model" => "UPDATE face_prints SET embedding_model='other-weights' WHERE id=2",
            "processing" => "UPDATE face_prints SET processing_version='macos-other-crops' WHERE id=2",
            "legacy" => "UPDATE face_prints SET embedding_model=NULL WHERE id=2",
            "revision" => "UPDATE face_prints SET source_revision='stale' WHERE id=2",
            _ => "UPDATE face_prints SET arcface_embedding=?1 WHERE id=2",
        };
        if fault == "nan" {
            catalog.conn.execute(fault_sql, [encode(&[f32::NAN; DIM])]).unwrap();
        } else {
            catalog.conn.execute(fault_sql, []).unwrap();
        }
        let partition = load_partition(&catalog.conn, &space(), &HashSet::new(), 0.0, MAX_FACES_PER_RUN).unwrap();
        assert_eq!((partition.rows.len(), partition.deferred), (1, 1), "{fault}");
        let mut conn = catalog.conn;
        assert_eq!(auto_merge(&mut conn, &space()).unwrap(), 0, "{fault}");
        let catalog = Catalog { conn, dir: catalog.dir };
        assert_eq!(catalog.assignments(), vec![(1, Some(10)), (2, Some(20))], "{fault}");
        let (catalog, outcome) = catalog.run();
        assert!(outcome.error.is_none(), "{fault}");
        assert_eq!(outcome.result.deferred_faces, Some(1), "{fault}");
        assert_eq!(catalog.assignments(), vec![(1, Some(10)), (2, Some(20))], "{fault}");
        assert_eq!(person_ids(&catalog), vec![10, 20], "{fault}");
        let name: String = catalog.conn.query_row("SELECT name FROM persons WHERE id=10", [], |row| row.get(0)).unwrap();
        assert_eq!(name, "Confirmed", "{fault}");
    }
}

#[test]
fn a_bridge_face_cannot_chain_two_named_people_together() {
    let catalog = Catalog::new();
    catalog.person(5, None, false);
    catalog.person(10, Some("Adam"), false);
    catalog.person(20, Some("Bob"), false);
    catalog.face(1, 0, 0.6, Some(10));
    catalog.face(2, 0, 0.6, Some(20));
    catalog.face(3, 0, 0.6, Some(5));
    let mut conn = catalog.conn;
    let absorbed = auto_merge(&mut conn, &space()).unwrap();
    let catalog = Catalog { conn, dir: catalog.dir };
    assert_eq!(absorbed, 1, "only the unnamed bridge is absorbed");
    let assignments = catalog.assignments();
    assert_eq!(&assignments[..2], &[(1, Some(10)), (2, Some(20))], "neither named person loses a face");
    assert!(matches!(assignments[2].1, Some(10 | 20)));
    let names: Vec<String> = catalog.conn.prepare("SELECT name FROM persons WHERE name IS NOT NULL ORDER BY id").unwrap().query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(names, vec!["Adam", "Bob"]);
}

#[test]
fn people_named_only_by_a_title_middle_name_or_suffix_are_never_auto_merged() {
    let catalog = Catalog::new();
    catalog.conn.execute("INSERT INTO persons(id,title,is_unknown,created_at,file_count) VALUES(10,'Dr.',0,0,0)", []).unwrap();
    catalog.conn.execute("INSERT INTO persons(id,middle_name,suffix,is_unknown,created_at,file_count) VALUES(20,'Quincy','Jr.',0,0,0)", []).unwrap();
    for face in 1..=3 {
        catalog.face(face, 0, 0.6, Some(10));
    }
    for face in 4..=6 {
        catalog.face(face, 0, 0.6, Some(20));
    }
    let mut conn = catalog.conn;
    assert_eq!(auto_merge(&mut conn, &space()).unwrap(), 0);
    let catalog = Catalog { conn, dir: catalog.dir };
    assert_eq!(owners(&catalog), vec![Some(10), Some(10), Some(10), Some(20), Some(20), Some(20)]);
}

/// `count` unnamed people, one face each, all the same identity: every pair of them qualifies.
fn crowd(count: i64) -> Catalog {
    let catalog = Catalog::new();
    let tx = catalog.conn.unchecked_transaction().unwrap();
    for id in 1..=count {
        catalog.person(id, None, false);
        catalog.face(id, 0, 0.6, Some(id));
    }
    tx.commit().unwrap();
    catalog
}

#[test]
fn a_merge_search_over_its_edge_budget_merges_no_one() {
    // 1,415 people give 1,000,405 qualifying pairs, one more than the 1,000,000-edge budget allows.
    let catalog = crowd(1415);
    let before = catalog.assignments();
    let mut conn = catalog.conn;
    assert_eq!(auto_merge(&mut conn, &space()).unwrap(), 0);
    let catalog = Catalog { conn, dir: catalog.dir };
    assert_eq!(catalog.assignments(), before, "a search that ran out of budget changes nothing");
    let file_counts: i64 = catalog.conn.query_row("SELECT COUNT(*) FROM persons WHERE file_count=0", [], |row| row.get(0)).unwrap();
    assert_eq!(file_counts, 1415, "no person row was touched");

    let catalog = crowd(1414);
    let mut conn = catalog.conn;
    assert_eq!(auto_merge(&mut conn, &space()).unwrap(), 1413, "inside the budget the same crowd is one person");
}

#[test]
fn an_exhausted_join_budget_returns_no_partial_graph() {
    let same: Vec<Vec<f32>> = vec![decode(&vector(0, 1, 0.0)); 3];
    let small = vec![false; 3];
    let join = |vectors: &[Vec<f32>], small: &[bool], evaluations: usize, found: Vec<(f32, usize, usize)>| {
        Join { vectors, small, tight: 0.65, loose: 0.55, evaluations, exceeded: false, found }.edges()
    };
    assert_eq!(join(&same, &small, 0, Vec::new()).map(|edges| edges.len()), Some(3));
    assert_eq!(join(&same, &small, JOIN_EVALUATIONS - 2, Vec::new()), None, "direct sweep out of work");
    assert_eq!(join(&same, &small, 0, vec![(1.0, 0, 0); JOIN_EDGES - 1]), None, "direct sweep out of edges");

    let dense: Vec<Vec<f32>> = vec![decode(&vector(0, 1, 0.0)); 720];
    let small = vec![false; dense.len()];
    assert!(dense.len() * (dense.len() - 1) / 2 > JOIN_DIRECT_PAIRS, "the vantage-point path");
    assert_eq!(join(&dense, &small, 0, Vec::new()).map(|edges| edges.len()), Some(720 * 719 / 2));
    assert_eq!(join(&dense, &small, JOIN_EVALUATIONS - 100, Vec::new()), None, "tree search out of work");
    assert_eq!(join(&dense, &small, 0, vec![(1.0, 0, 0); JOIN_EDGES - 100]), None, "tree search out of edges");
}

#[test]
fn pairs_at_the_exact_thresholds_are_joined_the_same_by_the_tree_and_the_direct_predicate() {
    const WIDE: usize = 512;
    let (tight, loose) = (0.65f32, 0.55f32);
    let along = |cosine: f32| {
        let mut values = vec![0.0f32; WIDE];
        values[0] = cosine;
        values[1] = (1.0 - cosine * cosine).max(0.0).sqrt();
        values
    };
    let mut vectors = vec![along(1.0)];
    for cosine in [tight.next_up(), tight, tight.next_down(), loose.next_up(), loose, loose.next_down()] {
        vectors.push(along(cosine));
    }
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    while vectors.len() < 720 {
        let mut values: Vec<f32> = (0..WIDE)
            .map(|axis| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                if axis < 2 { 0.0 } else { (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5 }
            })
            .collect();
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        for value in &mut values {
            *value /= norm;
        }
        vectors.push(values);
    }
    let mut small = vec![false; vectors.len()];
    small[4..=6].fill(true);
    assert!(vectors.len() * (vectors.len() - 1) / 2 > JOIN_DIRECT_PAIRS, "the vantage-point path");
    let tree = Join { vectors: &vectors, small: &small, tight, loose, evaluations: 0, exceeded: false, found: Vec::new() }.edges().unwrap();
    let mut direct = Vec::new();
    for i in 0..vectors.len() {
        for j in i + 1..vectors.len() {
            let cosine = dot(&vectors[i], &vectors[j]);
            if cosine >= tight || (cosine >= loose && (small[i] || small[j])) {
                direct.push((cosine, i, j));
            }
        }
    }
    direct.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let with_base: Vec<usize> = tree.iter().filter(|(_, i, _)| *i == 0).map(|(_, _, j)| *j).collect();
    assert_eq!(with_base, vec![1, 2, 4, 5], "at or above tight, or at or above loose for a small person, and nothing below");
    assert_eq!(tree, direct);
}

#[test]
fn a_photo_changed_after_the_partition_was_read_refuses_the_commit() {
    let catalog = Catalog::new();
    catalog.person(10, Some("Maya"), false);
    catalog.face(1, 0, 0.6, Some(10));
    let selected = load_partition(&catalog.conn, &space(), &HashSet::new(), 0.0, MAX_FACES_PER_RUN).unwrap();
    assert_eq!(selected.rows.len(), 1);
    assert!(require_unchanged(&catalog.conn, &selected, &space(), &HashSet::new(), 0.0).is_ok());
    std::fs::write(catalog.dir.0.join("1.jpg"), [1u8, 2]).unwrap();
    assert!(require_unchanged(&catalog.conn, &selected, &space(), &HashSet::new(), 0.0).is_err());
    let current = load_partition(&catalog.conn, &space(), &HashSet::new(), 0.0, MAX_FACES_PER_RUN).unwrap();
    assert_eq!((current.rows.len(), current.deferred), (0, 1));
    assert_eq!(person_ids(&catalog), vec![10]);
}

#[test]
fn faces_of_people_marked_unknown_do_not_use_up_the_face_window() {
    let catalog = Catalog::new();
    catalog.person(10, None, true);
    catalog.person(20, None, false);
    catalog.face(1, 0, 0.6, Some(10));
    catalog.face(2, 1, 0.6, Some(20));
    catalog.conn.execute("UPDATE persons SET is_unknown=0", []).unwrap();
    let loaded = load_partition(&catalog.conn, &space(), &HashSet::from([1]), 0.0, 1).unwrap();
    assert_eq!(loaded.rows.iter().map(|row| row.id).collect::<Vec<_>>(), vec![2]);
    assert!(!loaded.overflowed);
}

#[test]
fn a_pass_with_nothing_to_cluster_still_reports_the_people_it_kept() {
    let catalog = Catalog::new();
    catalog.person(10, None, true);
    catalog.face(1, 0, 0.6, Some(10));
    let (catalog, outcome) = catalog.run();
    assert!(outcome.error.is_none());
    assert_eq!((outcome.result.person_count, outcome.result.face_count), (1, 0));
    assert_eq!(catalog.assignments(), vec![(1, Some(10))]);
}

#[test]
fn oversized_evidence_is_deferred_while_other_people_still_merge() {
    let catalog = Catalog::new();
    for (person, face) in [(10, 1), (20, 2), (30, 3)] {
        catalog.person(person, None, false);
        catalog.face(face, 0, 0.6, Some(person));
    }
    let oversized = encode(&[1.0f32; 4097]);
    catalog.conn.execute("UPDATE face_prints SET arcface_embedding=?1 WHERE id=3", [&oversized]).unwrap();
    let mut conn = catalog.conn;
    assert_eq!(auto_merge(&mut conn, &space()).unwrap(), 1);
    let catalog = Catalog { conn, dir: catalog.dir };
    let assignments = catalog.assignments();
    assert_eq!(assignments[0].1, assignments[1].1);
    assert_eq!(assignments[2], (3, Some(30)), "the deferred face keeps its person");
    let bytes: i64 = catalog.conn.query_row("SELECT LENGTH(arcface_embedding) FROM face_prints WHERE id=3", [], |row| row.get(0)).unwrap();
    assert_eq!(bytes, 4097 * 4, "and its evidence");
    assert_eq!(person_ids(&catalog), vec![10, 20, 30], "auto-merge keeps every person row");
}

#[test]
fn a_plan_that_would_pass_the_person_limit_writes_nothing() {
    let catalog = Catalog::new();
    catalog.face(1, 0, 0.6, None);
    catalog.face(2, 1, 0.6, None);
    let tx = catalog.conn.unchecked_transaction().unwrap();
    for id in 100..100 + MAX_PERSONS as i64 - 1 {
        tx.execute("INSERT INTO persons(id,is_unknown,created_at,file_count) VALUES(?1,0,0,0)", [id]).unwrap();
    }
    tx.commit().unwrap();
    let plan = |faces: &[i64]| -> Vec<Planned> {
        faces
            .iter()
            .map(|face| Planned { person: None, faces: vec![*face], representative: *face, centroid: decode(&vector(0, 1, 0.0)), radius: 0.1 })
            .collect()
    };
    let pool = HashSet::from([1, 2]);
    let tx = catalog.conn.unchecked_transaction().unwrap();
    let refused = apply(&tx, &plan(&[1, 2]), &pool, &HashSet::new(), 1.0);
    assert!(refused.is_err(), "8,001 people");
    drop(tx);
    assert_eq!(person_ids(&catalog).len(), MAX_PERSONS - 1);
    assert_eq!(catalog.assignments(), vec![(1, None), (2, None)]);
    let tx = catalog.conn.unchecked_transaction().unwrap();
    assert_eq!(apply(&tx, &plan(&[1]), &pool, &HashSet::new(), 1.0).unwrap(), MAX_PERSONS as i64, "exactly the limit fits");
}

#[test]
fn a_persist_that_fails_part_way_leaves_every_person_and_assignment_as_it_was() {
    let catalog = Catalog::new();
    for face in 1..=4 {
        catalog.face(face, 0, 0.6, None);
    }
    for face in 5..=8 {
        catalog.face(face, 1, 0.6, None);
    }
    // The recount is the last statement of the write, after the new people and their faces.
    catalog
        .conn
        .execute_batch("CREATE TEMP TRIGGER refuse_recount BEFORE UPDATE OF file_count ON persons BEGIN SELECT RAISE(ABORT,'refused'); END;")
        .unwrap();
    let (catalog, outcome) = catalog.run();
    assert_eq!(outcome.error.map(|(kind, _)| kind), Some("face_cluster_persist_failed"));
    assert!(person_ids(&catalog).is_empty(), "the people created before the failure were rolled back");
    assert!(catalog.assignments().iter().all(|(_, person)| person.is_none()), "and so were their faces");
    catalog.conn.execute_batch("DROP TRIGGER refuse_recount").unwrap();
    let (catalog, outcome) = catalog.run();
    assert!(outcome.error.is_none());
    assert_eq!(person_ids(&catalog).len(), 2, "with the refusal gone the same pass publishes");
    assert!(catalog.assignments().iter().all(|(_, person)| person.is_some()));
}
