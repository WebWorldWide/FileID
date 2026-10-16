use crate::ipc::CatalogHit;
use anyhow::{bail, Result};
use rusqlite::{params, Connection, Row};

fn hit(row: &Row<'_>) -> rusqlite::Result<CatalogHit> {
    Ok(CatalogHit {
        file_id: row.get(0)?,
        path: row.get(1)?,
        kind: row.get(2)?,
        text: row.get(3)?,
        evidence_id: row.get(4)?,
        start_seconds: row.get(5)?,
        page: row.get(6)?,
    })
}
pub(super) fn search(
    conn: &Connection,
    query: &str,
    kinds: &[String],
    people: &[i64],
    events: &[String],
    time: Option<f64>,
) -> Result<Vec<CatalogHit>> {
    if !kinds
        .iter()
        .all(|kind| ["image", "video", "pdf", "doc", "audio", "other", "model"].contains(&kind.as_str()))
        || people.iter().any(|id| *id <= 0)
        || events.iter().any(|id| id.is_empty() || id.chars().count() > 200)
        || time.is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        bail!("Invalid catalog filters")
    }
    let kinds = if kinds.is_empty() {
        None
    } else {
        Some(serde_json::to_string(kinds)?)
    };
    let people = if people.is_empty() {
        None
    } else {
        Some(serde_json::to_string(people)?)
    };
    let events = if events.is_empty() {
        None
    } else {
        Some(serde_json::to_string(events)?)
    };
    let quoted = query
        .split_whitespace()
        .filter(|s| s.chars().any(char::is_alphanumeric))
        .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ");
    if quoted.is_empty() && kinds.is_none() && people.is_none() && events.is_none() && time.is_none() {
        return Ok(vec![]);
    }
    let filters = "f.failed=0 AND (?2 IS NULL OR f.kind IN (SELECT value FROM json_each(?2))) AND (?3 IS NULL OR f.id IN (SELECT file_id FROM face_prints WHERE person_id IN (SELECT value FROM json_each(?3)) UNION SELECT file_id FROM catalog_observations WHERE person_id IN (SELECT value FROM json_each(?3)) AND stale=0)) AND (?4 IS NULL OR f.id IN (SELECT file_id FROM catalog_event_files WHERE event_id IN (SELECT value FROM json_each(?4))))";
    let mut hits = Vec::new();
    if time.is_none() {
        let (from, predicate, order) = if quoted.is_empty() {
            ("files f", "", "f.id DESC")
        } else {
            (
                "catalog_file_fts JOIN files f ON f.id=catalog_file_fts.rowid",
                " AND catalog_file_fts MATCH ?1",
                "bm25(catalog_file_fts),f.id",
            )
        };
        let sql = format!("SELECT f.id,f.path_text,f.kind,COALESCE(f.vlm_description,''),NULL,NULL,NULL FROM {from} WHERE {filters}{predicate} AND ?1 IS NOT NULL ORDER BY {order} LIMIT 100");
        hits.extend(
            conn.prepare(&sql)?
                .query_map(params![quoted, kinds, people, events], hit)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    if !quoted.is_empty() || time.is_some() {
        let predicate = if quoted.is_empty() {
            ""
        } else {
            " AND catalog_evidence_fts MATCH ?1"
        };
        let order = if quoted.is_empty() {
            "COALESCE(c.start_seconds,p.start_seconds),e.evidence_id"
        } else {
            "bm25(catalog_evidence_fts),e.evidence_id"
        };
        // Empty temporal queries require timed observations, not a face elsewhere in the file.
        let temporal_people = if quoted.is_empty() {
            " AND (?3 IS NULL OR f.id IN (SELECT file_id FROM catalog_observations WHERE person_id IN (SELECT value FROM json_each(?3)) AND stale=0 AND start_seconds IS NOT NULL AND ((start_seconds<=?5 AND COALESCE(end_seconds,start_seconds)>=?5) OR ABS(start_seconds-?5)<=15)))"
        } else {
            ""
        };
        let sql = format!("SELECT f.id,f.path_text,CASE WHEN p.start_seconds IS NOT NULL AND p.confidence=0 THEN 'sampledFrame' ELSE e.kind END,e.text,e.evidence_id,COALESCE(c.start_seconds,p.start_seconds),p.page FROM catalog_evidence_fts e JOIN files f ON f.id=CAST(e.file_id AS INTEGER) LEFT JOIN catalog_chapters c ON c.id=e.evidence_id AND e.kind='chapter' LEFT JOIN catalog_passages p ON p.id=e.evidence_id AND e.kind='passage' WHERE {filters}{predicate}{temporal_people} AND ?1 IS NOT NULL AND (c.stale=0 OR p.stale=0) AND (?5 IS NULL OR ((c.start_seconds<=?5 AND c.end_seconds>=?5) OR ABS(c.start_seconds-?5)<=15 OR (p.start_seconds<=?5 AND COALESCE(p.end_seconds,p.start_seconds)>=?5) OR ABS(p.start_seconds-?5)<=15)) ORDER BY {order} LIMIT 100");
        hits.extend(
            conn.prepare(&sql)?
                .query_map(params![quoted, kinds, people, events, time], hit)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    if people.is_some() || time.is_some() {
        let sql = "SELECT f.id,f.path_text,f.kind,'Person: ' || COALESCE(NULLIF(trim(COALESCE(p.first_name,'') || ' ' || COALESCE(p.middle_name,'') || ' ' || COALESCE(p.last_name,'') || ' ' || COALESCE(p.suffix,'')),''),p.name,'Person') || CASE WHEN o.user_edited=1 THEN ' · user-confirmed appearance' ELSE ' · unverified appearance' END,'observation:' || o.id,o.start_seconds,NULL FROM catalog_observations o JOIN persons p ON p.id=o.person_id JOIN files f ON f.id=o.file_id WHERE f.failed=0 AND f.kind='video' AND (?4 IS NULL OR f.kind IN (SELECT value FROM json_each(?4))) AND o.stale=0 AND o.start_seconds IS NOT NULL AND (?1 IS NULL OR o.person_id IN (SELECT value FROM json_each(?1))) AND (?2 IS NULL OR f.id IN (SELECT file_id FROM catalog_event_files WHERE event_id IN (SELECT value FROM json_each(?2)))) AND (?3 IS NULL OR ((o.start_seconds<=?3 AND COALESCE(o.end_seconds,o.start_seconds)>=?3) OR ABS(o.start_seconds-?3)<=15)) ORDER BY f.id,o.start_seconds,o.id LIMIT 100";
        hits.extend(
            conn.prepare(sql)?
                .query_map(params![people, events, time, kinds], hit)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    if !quoted.is_empty() && time.is_none() {
        let seen: std::collections::HashSet<i64> = hits.iter().map(|h| h.file_id).collect();
        hits.extend(tag_hits(conn, query, filters, (&kinds, &people, &events))?.into_iter().filter(|h| !seen.contains(&h.file_id)));
        for (table, kind) in [("doc_fts", "documentText"), ("ocr_fts", "ocrText")] {
            let sql = format!(
                "SELECT f.id,f.path_text,'{kind}',snippet({table},0,'','','…',16),'{kind}:' || f.id,NULL,NULL FROM {table} JOIN files f ON f.id={table}.rowid WHERE {filters} AND {table} MATCH ?1 ORDER BY bm25({table}) LIMIT 100"
            );
            hits.extend(
                conn.prepare(&sql)?
                    .query_map(params![quoted, kinds, people, events], hit)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
    }
    Ok(hits)
}

/// Files whose scan tags match the query words, best first. Each word matches
/// the start of a tag word ("grass" hits "grassland", "sky" hits "blue_sky"),
/// and only files matching the most words are kept so a filler word in the
/// question can't hide a real match. Mirrors macOS `CatalogStore.tagHits`.
fn tag_hits(
    conn: &Connection,
    query: &str,
    filters: &str,
    (kinds, people, events): (&Option<String>, &Option<String>, &Option<String>),
) -> Result<Vec<CatalogHit>> {
    let mut words: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_owned)
        .collect();
    words.sort();
    words.dedup();
    words.truncate(6);
    if words.is_empty() {
        return Ok(vec![]);
    }
    let score = (0..words.len())
        .map(|i| format!("EXISTS (SELECT 1 FROM tags t WHERE t.file_id=f.id AND t.source IN ('auto','user','vlm') AND (' ' || REPLACE(LOWER(t.tag),'_',' ')) LIKE ?{})", i + 5))
        .collect::<Vec<_>>()
        .join(" + ");
    let sql = format!(
        "WITH scored AS (SELECT f.id,f.path_text,f.kind,{score} AS score FROM files f WHERE {filters} AND ?1 IS NOT NULL) \
         SELECT s.id,s.path_text,s.kind,'Tags: ' || COALESCE((SELECT GROUP_CONCAT(tag,', ') FROM (SELECT DISTINCT TRIM(tag) AS tag FROM tags WHERE file_id=s.id AND source IN ('auto','user','vlm') ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END,COALESCE(score,0) DESC LIMIT 8)),''),NULL,NULL,NULL \
         FROM scored s WHERE s.score>0 AND s.score=(SELECT MAX(score) FROM scored) ORDER BY s.id DESC LIMIT 100"
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(query.to_owned()), Box::new(kinds.clone()), Box::new(people.clone()), Box::new(events.clone())];
    for word in &words {
        args.push(Box::new(format!("% {word}%")));
    }
    Ok(conn
        .prepare(&sql)?
        .query_map(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())), hit)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) type KnownPeople = Vec<(i64, Vec<String>)>;
pub(super) type KnownEvents = Vec<(String, Vec<String>)>;
pub(super) fn known(conn: &Connection) -> Result<(KnownPeople, KnownEvents)> {
    let people = conn.prepare("SELECT id,name,title,first_name,middle_name,last_name,suffix FROM persons WHERE COALESCE(is_unknown,0)=0 ORDER BY id")?.query_map([], |row| {
        let fields = (2..7).map(|index| row.get::<_,Option<String>>(index)).collect::<rusqlite::Result<Vec<_>>>()?;
        let structured = fields.into_iter().flatten().filter(|value| !value.trim().is_empty()).collect::<Vec<_>>().join(" ");
        let first: Option<String> = row.get(3)?;
        let legacy: Option<String> = row.get(1)?;
        Ok((row.get(0)?, [Some(structured),first,legacy].into_iter().flatten().filter(|value| !value.trim().is_empty()).collect()))
    })?.collect::<rusqlite::Result<_>>()?;
    let events = conn.prepare("SELECT id,title FROM catalog_events WHERE user_edited=1 AND length(trim(title))>0 ORDER BY title LIMIT 200")?.query_map([], |row| Ok((row.get(0)?,vec![row.get(1)?])))?.collect::<rusqlite::Result<_>>()?;
    Ok((people, events))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scanned_tags_answer_without_descriptions() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        for id in 1..=3 {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?1,?2,?1,100,0,'image','jpg')", params![id, format!("/fixture/IMG_{id}.jpg")]).unwrap();
        }
        conn.execute("INSERT INTO tags(file_id,tag,source,score) VALUES(1,'grassland','auto',0.9),(1,'dog','auto',0.8),(2,'blue_sky','auto',0.9),(2,'grass','auto',0.7),(3,'grassy_knoll','rejected',0.9)", []).unwrap();
        let ids = |q: &str| search(&conn, q, &[], &[], &[], None).unwrap().into_iter().map(|h| h.file_id).collect::<Vec<_>>();
        assert_eq!(ids("photos with grass"), vec![2, 1]);
        assert_eq!(ids("dog on grass"), vec![1], "files matching every word outrank partial matches");
        assert_eq!(ids("sky"), vec![2]);
        assert!(ids("ass").is_empty(), "matches start at a tag word");
        let hits = search(&conn, "grass", &["video".into()], &[], &[], None).unwrap();
        assert!(hits.is_empty(), "kind filter applies");
        assert!(search(&conn, "grass", &[], &[], &[], None).unwrap()[1].text.starts_with("Tags: grassland"));
    }
    #[test]
    fn filters_apply_before_limits_and_exclude_failed_stale_and_unknown() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        for id in 1..=150 {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(?1,?2,?1,100,0,'video','mov','Birthday gifts')",params![id,format!("/fixture/{id}.mov")]).unwrap();
        }
        conn.execute(
            "INSERT INTO persons(id,name,created_at,is_unknown) VALUES(1,'Alice',0,0),(2,'Unknown',0,1)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO catalog_events(id,title,user_edited) VALUES('event','Family Birthday',1),('automatic','Guessed',0)",[]).unwrap();
        conn.execute(
            "INSERT INTO catalog_event_files(event_id,file_id) VALUES('event',1)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO catalog_observations(id,file_id,person_id,start_seconds,end_seconds,source_revision,model_version,confidence,user_edited) VALUES('alice',1,1,120,125,'fixture','user',1,1)",[]).unwrap();
        conn.execute("INSERT INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES('near',1,121,124,'Birthday gifts','fixture','fixture',0),('far',1,600,610,'Birthday gifts','fixture','fixture',0)",[]).unwrap();
        assert_eq!(
            known(&conn).unwrap(),
            (
                vec![(1, vec!["Alice".into()])],
                vec![("event".into(), vec!["Family Birthday".into()])]
            )
        );
        let hits = search(&conn, "Birthday", &[], &[1], &["event".into()], Some(123.0)).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].evidence_id.as_deref(), Some("near"));
        assert!(hits[1].text.contains("user-confirmed"));
        assert!(hits.iter().all(|h| h.file_id == 1));
        assert_eq!(
            search(&conn, "", &[], &[1], &["event".into()], Some(123.0))
                .unwrap()
                .len(),
            2
        );
        for kind in ["image", "audio", "doc", "pdf"] {
            for (query, time) in [("", None), ("Birthday", Some(123.0)), ("", Some(123.0))] {
                assert!(
                    search(&conn, query, &[kind.into()], &[1], &[], time)
                        .unwrap()
                        .is_empty(),
                    "{kind} observation filter"
                );
            }
        }
        conn.execute("UPDATE catalog_passages SET stale=1 WHERE file_id=1", [])
            .unwrap();
        assert_eq!(search(&conn, "Birthday", &[], &[1], &[], Some(123.0)).unwrap().len(), 1);
        conn.execute("UPDATE files SET failed=1 WHERE id=1", []).unwrap();
        assert!(search(&conn, "Birthday", &[], &[1], &[], None).unwrap().is_empty());
        assert!(search(&conn, "", &["video".into()], &[1], &[], None)
            .unwrap()
            .is_empty());
        assert!(search(&conn, "", &[], &[1], &[], Some(123.0)).unwrap().is_empty());
    }
}
