use crate::{
    ipc::{CatalogHit, CatalogRequest},
    models::clip_space,
};
use anyhow::{bail, Result};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

pub(crate) fn validate(request: &CatalogRequest) -> Result<(&str, &str, usize, bool)> {
    let mode = request.search_mode.as_deref().unwrap_or("keyword");
    let query = request.query.as_deref().unwrap_or_default().trim();
    let scope = request.result_scope.as_deref().unwrap_or("all");
    let limit = request.limit.unwrap_or(100);
    if request.request_id.is_empty()
        || request.request_id.chars().count() > 200
        || !["keyword", "semantic", "hybrid"].contains(&mode)
        || !["all", "files"].contains(&scope)
        || !(1..=100).contains(&limit)
        || query.chars().count() > 2000
    {
        bail!("Invalid catalog search")
    }
    if mode == "keyword" {
        if query.is_empty() || request.query_vector.is_some() || request.embedding_model.is_some() {
            bail!("Invalid keyword search")
        }
    } else if let Some(vector) = &request.query_vector {
        if request.file_id.is_some()
            || request.embedding_model.as_deref() != Some(clip_space::model_id())
            || !clip_space::valid(vector)
        {
            bail!("Invalid visual query vector or model")
        }
    } else if mode != "semantic" || request.file_id.is_none() || request.embedding_model.is_some() {
        bail!("A current-model vector or semantic seed file is required")
    }
    if mode == "hybrid" && query.is_empty() {
        bail!("Hybrid search requires a keyword query")
    }
    Ok((mode, query, limit, scope == "files"))
}

pub(crate) fn vector(conn: &Connection, request: &CatalogRequest) -> Result<Vec<f32>> {
    if let Some(vector) = &request.query_vector {
        return Ok(vector.clone());
    }
    let blob: Vec<u8> = conn.query_row("SELECT e.embedding FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.file_id=?1 AND e.model=?2 AND f.failed=0", params![request.file_id,clip_space::model_id()], |r| r.get(0))?;
    clip_space::decode(&blob)
        .ok_or_else(|| anyhow::anyhow!("Invalid or unavailable current-model seed embedding"))
}

pub(crate) fn select(hits: Vec<CatalogHit>, limit: usize, files_only: bool) -> Vec<CatalogHit> {
    let mut seen = HashSet::new();
    hits.into_iter()
        .filter(|hit| !files_only || seen.insert(hit.file_id))
        .take(limit)
        .collect()
}

pub(crate) fn merge(
    keyword: Vec<CatalogHit>,
    visual: Vec<CatalogHit>,
    limit: usize,
    files_only: bool,
) -> Vec<CatalogHit> {
    let mut ranked: Vec<(CatalogHit, f64)> = Vec::new();
    let mut positions = HashMap::new();
    for list in [keyword, visual] {
        for (rank, hit) in list.into_iter().enumerate() {
            let key = hit
                .evidence_id
                .as_ref()
                .map(|id| format!("evidence:{id}"))
                .unwrap_or_else(|| format!("file:{}", hit.file_id));
            let next = ranked.len();
            let index = *positions.entry(key).or_insert(next);
            let score = 1.0 / (61 + rank) as f64;
            if index == next {
                ranked.push((hit, score));
            } else {
                ranked[index].1 += score;
            }
        }
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    select(
        ranked.into_iter().map(|(hit, _)| hit).collect(),
        limit,
        files_only,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(value: serde_json::Value) -> CatalogRequest {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn strict_vector_model_seed_modes_limits_and_scope() {
        let mut v = vec![0.0; 512];
        v[0] = 1.0;
        let valid = serde_json::json!({"requestID":"r","action":"search","searchMode":"hybrid","query":"birthday","queryVector":v,"embeddingModel":clip_space::model_id(),"resultScope":"files"});
        assert!(validate(&request(valid.clone())).is_ok());
        for (field, value) in [
            ("embeddingModel", serde_json::json!("mobileclip_s2")),
            ("queryVector", serde_json::json!([1.0])),
            ("queryVector", serde_json::json!(vec![0.0; 512])),
            ("fileID", serde_json::json!(1)),
            ("query", serde_json::json!("")),
            ("limit", serde_json::json!(101)),
            ("resultScope", serde_json::json!("unknown")),
            ("searchMode", serde_json::json!("unknown")),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(validate(&request(invalid)).is_err(), "{field}");
        }
        assert!(validate(&request(serde_json::json!({"requestID":"r","action":"search","searchMode":"semantic","fileID":1}))).is_ok());
    }
    fn hit(file_id: i64, evidence: Option<&str>) -> CatalogHit {
        CatalogHit {
            file_id,
            evidence_id: evidence.map(str::to_owned),
            path: String::new(),
            kind: "image".into(),
            text: String::new(),
            start_seconds: None,
            page: None,
        }
    }
    #[test]
    fn reciprocal_rank_fusion_preserves_evidence_and_stable_ties() {
        let hits = merge(
            vec![hit(2, None), hit(1, Some("chapter")), hit(1, None)],
            vec![hit(1, None), hit(3, None)],
            100,
            false,
        );
        assert_eq!(
            hits.iter()
                .map(|h| (h.file_id, h.evidence_id.as_deref()))
                .collect::<Vec<_>>(),
            vec![(1, None), (2, None), (1, Some("chapter")), (3, None)]
        );
        assert_eq!(
            merge(
                vec![hit(2, None), hit(1, Some("chapter")), hit(1, None)],
                vec![hit(1, None), hit(3, None)],
                100,
                true
            )
            .len(),
            3
        );
    }
}
