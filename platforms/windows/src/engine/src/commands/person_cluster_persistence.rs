use crate::pipeline::face_clustering::{ClusterAnchor, ClusterAssignment, FaceRow};
use crate::pipeline::stable_person_assignments::{self, Cluster, Prior};
use rusqlite::OptionalExtension;
use std::collections::{BTreeMap, HashMap, HashSet};

fn different_pairs(conn: &rusqlite::Connection) -> anyhow::Result<Vec<(i64, i64)>> {
    let resolve = |legacy: Option<i64>,
                   file: Option<i64>,
                   bbox: Option<String>|
     -> anyhow::Result<Option<i64>> {
        if let (Some(file), Some(bbox)) = (file, bbox) {
            let mut query = conn.prepare(
                "SELECT id FROM face_prints WHERE file_id=?1 AND bbox=?2 ORDER BY id LIMIT 2",
            )?;
            let ids = query
                .query_map(rusqlite::params![file, bbox], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if ids.len() == 1 {
                return Ok(ids.first().copied());
            }
            anyhow::ensure!(
                ids.len() < 2,
                "Ambiguous face correction; existing People were preserved."
            );
        }
        let Some(id) = legacy else {
            return Ok(None);
        };
        Ok(conn
            .query_row("SELECT id FROM face_prints WHERE id=?1", [id], |r| r.get(0))
            .optional()?)
    };
    let mut pairs = Vec::new();
    let mut stmt = conn.prepare("SELECT face_a,face_b,file_a,bbox_a,file_b,bbox_b FROM face_verifications WHERE same_person=0")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if let (Some(a), Some(b)) = (
            resolve(row.get(0)?, row.get(2)?, row.get(3)?)?,
            resolve(row.get(1)?, row.get(4)?, row.get(5)?)?,
        ) {
            anyhow::ensure!(
                a != b,
                "Contradictory face correction; existing People were preserved."
            );
            pairs.push((a, b));
        }
    }
    Ok(pairs)
}

pub(super) fn persist(
    tx: &rusqlite::Transaction<'_>,
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    anchors: Vec<ClusterAnchor>,
) -> anyhow::Result<u32> {
    let mut priors = BTreeMap::new();
    let mut protected = HashSet::new();
    let mut preserved = HashSet::new();
    {
        let mut stmt = tx.prepare("SELECT id,COALESCE(is_unknown,0),COALESCE(trim(name),'') || COALESCE(trim(title),'') || COALESCE(trim(first_name),'') || COALESCE(trim(middle_name),'') || COALESCE(trim(last_name),'') || COALESCE(trim(suffix),'') FROM persons ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id = row.get::<_, i64>(0)?;
            priors.insert(
                id,
                Prior {
                    id,
                    face_ids: Vec::new(),
                },
            );
            if row.get::<_, i64>(1)? != 0 {
                preserved.insert(id);
            }
            if !row.get::<_, String>(2)?.is_empty() {
                protected.insert(id);
            }
        }
    }
    let mut owner = HashMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT id,person_id FROM face_prints WHERE person_id IS NOT NULL ORDER BY id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let (face, person) = (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?);
            if let Some(prior) = priors.get_mut(&person) {
                prior.face_ids.push(face);
                owner.insert(face, person);
            }
        }
    }
    let mut different = different_pairs(tx)?;
    let mut first_by_file = HashMap::new();
    for face in faces {
        if let Some(&first) = first_by_file.get(&face.file_id) {
            different.push((first, face.face_id));
        } else {
            first_by_file.insert(face.file_id, face.face_id);
        }
    }
    let cluster_of: HashMap<_, _> = assignments
        .iter()
        .map(|a| (a.face_id, a.cluster_id))
        .collect();
    let mut blocked = HashSet::new();
    for &(a, b) in &different {
        if let (Some(&a), Some(&b)) = (cluster_of.get(&a), cluster_of.get(&b)) {
            if a != b {
                blocked.insert((a.min(b), a.max(b)));
            }
        }
    }
    let names: HashMap<_, _> = owner
        .iter()
        .filter(|(_, p)| protected.contains(p))
        .map(|(&f, &p)| (f, p.to_string()))
        .collect();
    blocked.extend(crate::pipeline::face_clustering::name_blocked_pairs(
        &names,
        &cluster_of,
    ));
    blocked.extend(crate::pipeline::face_clustering::unknown_blocked_pairs(
        &owner,
        &preserved,
        &cluster_of,
    ));
    let (assignments, _) = crate::pipeline::face_clustering::consolidate(
        faces,
        assignments,
        anchors,
        &blocked,
        crate::pipeline::face_clustering::automerge_threshold(),
    );
    let mut raw = BTreeMap::<i32, Vec<i64>>::new();
    for a in assignments {
        raw.entry(a.cluster_id).or_default().push(a.face_id);
    }
    let excluded: HashSet<_> = owner
        .iter()
        .filter(|(_, p)| preserved.contains(p))
        .map(|(&f, _)| f)
        .collect();
    protected.extend(
        different
            .iter()
            .flat_map(|&(a, b)| [a, b])
            .filter_map(|f| owner.get(&f).copied()),
    );
    let protected_owner: HashMap<_, _> = owner
        .iter()
        .filter(|(_, p)| protected.contains(p) && !preserved.contains(p))
        .map(|(&f, &p)| (f, p))
        .collect();
    let buckets = stable_person_assignments::partition(
        &raw.into_values().collect::<Vec<_>>(),
        &protected_owner,
        &different,
        &excluded,
    );
    let by_face: HashMap<_, _> = faces.iter().map(|f| (f.face_id, f)).collect();
    let clusters: Vec<_> = buckets
        .into_iter()
        .map(|face_ids| {
            let representative = *face_ids
                .iter()
                .max_by(|&&a, &&b| {
                    by_face[&a]
                        .quality
                        .total_cmp(&by_face[&b].quality)
                        .then_with(|| b.cmp(&a))
                })
                .expect("nonempty partition");
            Cluster {
                face_ids,
                representative,
            }
        })
        .collect();
    let mut fixed = vec![None; clusters.len()];
    let mut claims = BTreeMap::<i64, (usize, usize)>::new();
    for (index, c) in clusters.iter().enumerate() {
        if let Some(&person) = protected_owner.get(&c.representative) {
            let entry = claims.entry(person).or_insert((index, c.face_ids.len()));
            if c.face_ids.len() > entry.1 {
                *entry = (index, c.face_ids.len());
            }
        }
    }
    for (person, (index, _)) in claims {
        fixed[index] = Some(person);
    }
    let ids = stable_person_assignments::resolve(
        &priors.into_values().collect::<Vec<_>>(),
        &clusters,
        &fixed,
        &preserved,
    )?;
    let existing: usize = tx.query_row("SELECT COUNT(*) FROM persons", [], |r| r.get(0))?;
    anyhow::ensure!(
        existing + ids.iter().filter(|id| id.is_none()).count() <= 8000,
        "Person limit reached; existing People were preserved."
    );
    let pool: HashSet<_> = faces.iter().map(|f| f.face_id).collect();
    tx.execute("UPDATE face_prints SET person_id=NULL WHERE id IN(SELECT value FROM json_each(?1)) AND COALESCE(excluded,0)=0 AND (person_id IS NULL OR person_id NOT IN(SELECT value FROM json_each(?2)))",rusqlite::params![serde_json::to_string(&pool)?,serde_json::to_string(&preserved)?])?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs_f64();
    for (c, id) in clusters.iter().zip(ids) {
        let mut centroid = vec![0.0f32; 128];
        for f in &c.face_ids {
            for (sum, v) in centroid.iter_mut().zip(&by_face[f].embedding) {
                *sum += v;
            }
        }
        let norm = centroid.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > f32::EPSILON {
            for v in &mut centroid {
                *v /= norm;
            }
        } else {
            centroid.clone_from(&by_face[&c.representative].embedding);
        }
        let blob: Vec<u8> = centroid.iter().flat_map(|v| v.to_le_bytes()).collect();
        let person = if let Some(id) = id {
            tx.execute("UPDATE persons SET representative_face_id=?1,centroid=?2,anchor_radius=0.5,last_clustered_at=?3 WHERE id=?4",rusqlite::params![c.representative,blob,now,id])?;
            id
        } else {
            tx.execute("INSERT INTO persons(representative_face_id,file_count,created_at,centroid,anchor_radius,last_clustered_at) VALUES(?1,0,?2,?3,0.5,?2)",rusqlite::params![c.representative,now,blob])?;
            tx.last_insert_rowid()
        };
        tx.execute(
            "UPDATE face_prints SET person_id=?1 WHERE id IN(SELECT value FROM json_each(?2))",
            rusqlite::params![person, serde_json::to_string(&c.face_ids)?],
        )?;
    }
    tx.execute("UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=persons.id)",[])?;
    tx.execute("UPDATE persons SET representative_face_id=(SELECT id FROM face_prints WHERE person_id=persons.id AND COALESCE(excluded,0)=0 ORDER BY COALESCE(face_quality,0) DESC,id LIMIT 1) WHERE representative_face_id IS NULL OR NOT EXISTS(SELECT 1 FROM face_prints WHERE id=persons.representative_face_id AND person_id=persons.id AND COALESCE(excluded,0)=0)",[])?;
    Ok(tx.query_row("SELECT COUNT(*) FROM persons", [], |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FixtureRoot(std::path::PathBuf);
    impl FixtureRoot {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for FixtureRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> anyhow::Result<(FixtureRoot, rusqlite::Connection, Vec<FaceRow>)> {
        let root = FixtureRoot(
            std::env::temp_dir().join(format!("fileid-stable-{}", uuid::Uuid::new_v4())),
        );
        let conn = crate::db::open_writer(&root.path().join("catalog.sqlite"))?;
        conn.execute_batch("INSERT INTO persons(id,first_name,is_unknown,created_at) VALUES(7,'Alex',0,123),(8,NULL,1,123),(9,'Offline',0,123); CREATE TABLE fixture_refs(person_id INTEGER REFERENCES persons(id)); INSERT INTO fixture_refs VALUES(7),(8),(9);")?;
        let mut faces = Vec::new();
        for id in 1..=4 {
            let person = match id {
                1 | 2 => Some(7),
                3 => Some(8),
                _ => None,
            };
            let mut embedding = vec![0.0f32; 128];
            embedding[0] = 1.0;
            let blob: Vec<u8> = embedding.iter().flat_map(|v| v.to_le_bytes()).collect();
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?1,?2,?1,1,0,'image','jpg')",rusqlite::params![id,root.path().join(format!("{id}.jpg")).to_string_lossy()])?;
            conn.execute("INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,face_quality,excluded) VALUES(?1,?1,?2,X'00','[0,0,1,1]',?3,1,0)",rusqlite::params![id,person,blob])?;
            faces.push(FaceRow {
                face_id: id,
                file_id: id,
                embedding,
                quality: 1.0,
            });
        }
        Ok((root, conn, faces))
    }
    fn run(conn: &rusqlite::Connection, faces: &[FaceRow]) -> anyhow::Result<u32> {
        let assignments = faces
            .iter()
            .map(|f| ClusterAssignment {
                face_id: f.face_id,
                cluster_id: 1,
            })
            .collect();
        let anchors = vec![ClusterAnchor {
            cluster_id: 1,
            anchor_face_id: faces[0].face_id,
            anchor_embedding: faces[0].embedding.clone(),
            member_count: faces.len() as u32,
        }];
        let tx = conn.unchecked_transaction()?;
        let count = persist(&tx, faces, assignments, anchors)?;
        tx.commit()?;
        Ok(count)
    }
    #[test]
    fn retains_ids_structured_names_unknown_offline_and_references() -> anyhow::Result<()> {
        let (_root, conn, faces) = fixture()?;
        assert_eq!(run(&conn, &faces)?, 4);
        assert_eq!(run(&conn, &faces)?, 4);
        assert_eq!(
            conn.query_row(
                "SELECT first_name,created_at FROM persons WHERE id=7",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            )?,
            ("Alex".into(), 123.0)
        );
        assert_eq!(
            conn.query_row("SELECT person_id FROM face_prints WHERE id=3", [], |r| r
                .get::<_, i64>(0))?,
            8
        );
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM fixture_refs JOIN persons ON persons.id=fixture_refs.person_id",[],|r|r.get::<_,i64>(0))?,3);
        assert_eq!(
            conn.query_row("SELECT file_count FROM persons WHERE id=7", [], |r| r
                .get::<_, i64>(0))?,
            2
        );
        Ok(())
    }
    #[test]
    fn failure_rolls_back_assignments_and_analysis() -> anyhow::Result<()> {
        let (_root, conn, faces) = fixture()?;
        conn.execute_batch("CREATE TRIGGER fixture_fail BEFORE INSERT ON persons BEGIN SELECT RAISE(ABORT,'fixture failure'); END;")?;
        assert!(run(&conn, &faces).is_err());
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM face_prints WHERE person_id=7",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            2
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM persons WHERE centroid IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM persons", [], |r| r.get::<_, i64>(0))?,
            3
        );
        Ok(())
    }
    #[test]
    fn fresh_negative_verdict_splits_a_raw_cluster() -> anyhow::Result<()> {
        let (_root, conn, faces) = fixture()?;
        conn.execute("INSERT INTO face_verifications(person_a,person_b,face_a,face_b,same_person,confidence,vlm_model,verified_at) VALUES(7,7,1,2,0,1,'fixture',0)",[])?;
        run(&conn, &faces)?;
        assert_ne!(
            conn.query_row("SELECT person_id FROM face_prints WHERE id=1", [], |r| r
                .get::<_, i64>(0))?,
            conn.query_row("SELECT person_id FROM face_prints WHERE id=2", [], |r| r
                .get::<_, i64>(0))?
        );
        Ok(())
    }
    #[test]
    fn same_image_faces_stay_separate_even_with_one_prior_owner() -> anyhow::Result<()> {
        let (_root, conn, mut faces) = fixture()?;
        conn.execute("UPDATE face_prints SET file_id=1 WHERE id=2", [])?;
        faces[1].file_id = 1;
        run(&conn, &faces)?;
        assert_ne!(
            conn.query_row("SELECT person_id FROM face_prints WHERE id=1", [], |r| r
                .get::<_, i64>(0))?,
            conn.query_row("SELECT person_id FROM face_prints WHERE id=2", [], |r| r
                .get::<_, i64>(0))?
        );
        Ok(())
    }
}
