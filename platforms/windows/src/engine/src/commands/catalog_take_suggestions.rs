use crate::ipc::{CatalogTakeGroupMember, CatalogTakeGroupSuggestion};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection};
use std::collections::{BTreeSet, HashSet};

const MAXIMUM_GAP: f64 = 20.0 * 60.0;
const MINIMUM_SIMILARITY: f32 = 0.90;
const MAXIMUM_ACTIVE_GROUPS: usize = 32;
const DIMENSION: usize = 512;

struct Group {
    anchor_time: f64,
    anchor: Vec<f32>,
    latest_time: f64,
    members: Vec<CatalogTakeGroupMember>,
    lowest_similarity: f32,
}

pub(super) fn discover(
    conn: &Connection,
    file_ids: &[i64],
    limit: usize,
) -> Result<Vec<CatalogTakeGroupSuggestion>> {
    let ids: BTreeSet<_> = file_ids.iter().copied().collect();
    ensure!(
        (2..=10_000).contains(&ids.len()) && ids.iter().all(|id| *id > 0) && (1..=100).contains(&limit),
        "Invalid take suggestion request"
    );
    let scope = serde_json::to_string(&ids)?;
    let mut statement = conn.prepare(
        "SELECT f.id, f.path_text, f.created_at, f.content_hash, e.embedding
         FROM files f
         JOIN clip_embeddings e ON e.file_id=f.id AND e.model=?1
         WHERE f.id IN (SELECT value FROM json_each(?2))
           AND f.kind IN ('image','video') AND f.failed=0 AND f.created_at IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM catalog_assets a WHERE a.derived_id=f.id)
           AND NOT EXISTS (SELECT 1 FROM catalog_event_files ef WHERE ef.file_id=f.id)
         ORDER BY f.created_at, f.id",
    )?;
    let mut rows = statement.query(params!["mobileclip_s2", scope])?;
    let mut active: Vec<Group> = Vec::new();
    let mut finished: Vec<Group> = Vec::new();
    let mut seen_hashes: HashSet<Vec<u8>> = HashSet::new();

    while let Some(row) = rows.next()? {
        let time: f64 = row.get(2)?;
        let blob: Vec<u8> = row.get(4)?;
        let Some(vector) = decode(&blob) else { continue };
        if !time.is_finite() { continue; }
        let member = CatalogTakeGroupMember { file_id: row.get(0)?, path: row.get(1)? };
        let hash: Option<Vec<u8>> = row.get(3)?;
        if hash.is_some_and(|value| !seen_hashes.insert(value)) { continue; }

        while active.first().is_some_and(|group| time - group.anchor_time > MAXIMUM_GAP) {
            finished.push(active.remove(0));
        }
        let mut best: Option<usize> = None;
        let mut best_similarity = MINIMUM_SIMILARITY;
        for (index, group) in active.iter().enumerate() {
            if group.members.len() >= 100 { continue; }
            let similarity = dot(&vector, &group.anchor);
            if similarity >= best_similarity {
                best = Some(index);
                best_similarity = similarity;
            }
        }
        if let Some(index) = best {
            let group = &mut active[index];
            group.members.push(member);
            group.latest_time = time;
            group.lowest_similarity = group.lowest_similarity.min(best_similarity);
        } else {
            if active.len() >= MAXIMUM_ACTIVE_GROUPS { finished.push(active.remove(0)); }
            active.push(Group {
                anchor_time: time,
                anchor: vector,
                latest_time: time,
                members: vec![member],
                lowest_similarity: 1.0,
            });
        }
    }
    finished.extend(active);
    finished.retain(|group| group.members.len() >= 2);
    finished.sort_by(|left, right| {
        right.latest_time.total_cmp(&left.latest_time)
            .then_with(|| left.members[0].file_id.cmp(&right.members[0].file_id))
    });
    Ok(finished.into_iter().take(limit).map(|group| CatalogTakeGroupSuggestion {
        members: group.members,
        similarity: f64::from(group.lowest_similarity),
        reason: "Similar visual content and file dates. Review the files and desired outcome before saving.".into(),
    }).collect())
}

fn decode(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() != DIMENSION * 4 { return None; }
    let vector: Vec<f32> = blob.as_chunks::<4>().0.iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();
    if !vector.iter().all(|value| value.is_finite()) { return None; }
    let norm: f64 = vector.iter().map(|value| f64::from(*value) * f64::from(*value)).sum();
    if (norm - 1.0).abs() > 0.02 { return None; }
    Some(vector)
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(first: f32, second: f32) -> Vec<u8> {
        let mut vector = vec![0.0_f32; DIMENSION];
        vector[0] = first;
        vector[1] = second;
        vector.into_iter().flat_map(f32::to_le_bytes).collect()
    }

    #[test]
    fn visual_grouping_skips_exact_duplicates_derived_versions_and_distant_takes() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        for (id, time, hash) in [(1, 1000.0, 1_u8), (2, 1060.0, 2), (3, 1090.0, 3),
                                 (4, 1120.0, 1), (5, 3000.0, 5), (6, 1080.0, 6), (7, 1070.0, 7)] {
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension,content_hash) VALUES(?1,?2,?3,100,?4,?4,0,'video','mov',?5)",
                params![id, format!("/internal/{id}.mov"), id, time, vec![hash]],
            ).unwrap();
        }
        let side = (1.0_f32 - 0.95_f32 * 0.95_f32).sqrt();
        for (id, first, second) in [(1, 1.0, 0.0), (2, 0.95, side), (3, 0.0, 1.0),
                                    (4, 1.0, 0.0), (5, 1.0, 0.0), (6, 1.0, 0.0), (7, 1.0, 0.0)] {
            conn.execute(
                "INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?1,?2,?3)",
                params![id, blob(first, second), if id == 7 { "old-model" } else { "mobileclip_s2" }],
            ).unwrap();
        }
        conn.execute("INSERT INTO catalog_assets(original_id,derived_id,role,recipe_json) VALUES(1,6,'export','{}')", []).unwrap();
        let groups = discover(&conn, &[1, 2, 3, 4, 5, 6, 7], 20).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.iter().map(|member| member.file_id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(groups[0].similarity >= 0.90);
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM catalog_events", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        let request = serde_json::from_value(serde_json::json!({
            "requestID": "suggest", "action": "suggestTakeGroups", "fileIDs": [1, 2, 3, 4, 5, 6, 7]
        })).unwrap();
        let response = crate::commands::catalog::execute(&mut conn, &request).unwrap();
        let wire = serde_json::to_value(response).unwrap();
        assert_eq!(wire["suggestedTakeGroups"][0]["members"][1]["fileID"], 2);
    }
}
