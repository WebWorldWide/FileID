use std::collections::{HashMap, HashSet};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Prior {
    pub id: i64,
    #[serde(rename = "faceIDs")]
    pub face_ids: Vec<i64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Cluster {
    #[serde(rename = "faceIDs")]
    pub face_ids: Vec<i64>,
    pub representative: i64,
}

pub(crate) fn resolve(
    priors: &[Prior],
    clusters: &[Cluster],
    fixed: &[Option<i64>],
    preserved: &HashSet<i64>,
) -> anyhow::Result<Vec<Option<i64>>> {
    anyhow::ensure!(fixed.len() == clusters.len(), "Invalid person partition");
    let mut owner = HashMap::new();
    let mut sizes = HashMap::new();
    for prior in priors {
        anyhow::ensure!(
            prior.id > 0 && sizes.insert(prior.id, prior.face_ids.len()).is_none(),
            "Invalid prior identity"
        );
        for &face in &prior.face_ids {
            anyhow::ensure!(
                face > 0 && owner.insert(face, prior.id).is_none(),
                "Invalid prior face ownership"
            );
        }
    }
    let mut selected = fixed.to_vec();
    let mut claimed = HashSet::new();
    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    for (index, cluster) in clusters.iter().enumerate() {
        anyhow::ensure!(
            !cluster.face_ids.is_empty() && cluster.face_ids.contains(&cluster.representative),
            "Invalid representative"
        );
        let mut counts = HashMap::<i64, usize>::new();
        for &face in &cluster.face_ids {
            anyhow::ensure!(face > 0 && seen.insert(face), "Repeated cluster face");
            if let Some(&person) = owner.get(&face) {
                anyhow::ensure!(
                    !preserved.contains(&person),
                    "Preserved face cannot be reassigned"
                );
                *counts.entry(person).or_default() += 1;
            }
        }
        if let Some(person) = fixed[index] {
            anyhow::ensure!(
                sizes.contains_key(&person)
                    && !preserved.contains(&person)
                    && claimed.insert(person)
                    && counts.get(&person).copied().unwrap_or(0) > 0,
                "Invalid fixed identity"
            );
        } else {
            for (person, overlap) in counts {
                let threshold = sizes.get(&person).copied().unwrap_or(0).div_ceil(2).max(1);
                if overlap >= threshold {
                    edges.push((
                        overlap,
                        owner.get(&cluster.representative) == Some(&person),
                        person,
                        index,
                    ));
                }
            }
        }
    }
    edges.sort_by_key(|&(overlap, owns_representative, person, cluster)| {
        (
            std::cmp::Reverse(overlap),
            std::cmp::Reverse(owns_representative),
            person,
            cluster,
        )
    });
    for (_, _, person, cluster) in edges {
        if selected[cluster].is_none() && claimed.insert(person) {
            selected[cluster] = Some(person);
        }
    }
    Ok(selected)
}

pub(crate) fn partition(
    raw: &[Vec<i64>],
    owners: &HashMap<i64, i64>,
    different: &[(i64, i64)],
    excluded: &HashSet<i64>,
) -> Vec<Vec<i64>> {
    let active: HashSet<i64> = raw
        .iter()
        .flatten()
        .copied()
        .filter(|f| !excluded.contains(f))
        .collect();
    let ownerless: std::collections::BTreeSet<i64> = different
        .iter()
        .flat_map(|&(a, b)| [a, b])
        .filter(|f| active.contains(f) && !owners.contains_key(f))
        .collect();
    let mut grouped = std::collections::BTreeMap::<i64, Vec<i64>>::new();
    for (&face, &person) in owners {
        if active.contains(&face) {
            grouped.entry(person).or_default().push(face);
        }
    }
    let mut singles = HashMap::<i64, HashSet<i64>>::new();
    for &(a, b) in different {
        if let (Some(&first), Some(&second)) = (owners.get(&a), owners.get(&b)) {
            if first == second {
                singles.entry(first).or_default().extend([a, b]);
            }
        }
    }
    let mut buckets = Vec::new();
    for (person, mut members) in grouped {
        members.sort_unstable();
        let individual = singles.remove(&person).unwrap_or_default();
        let remainder: Vec<i64> = members
            .iter()
            .copied()
            .filter(|f| !individual.contains(f))
            .collect();
        if !remainder.is_empty() {
            buckets.push(remainder);
        }
        for face in members.into_iter().filter(|f| individual.contains(f)) {
            buckets.push(vec![face]);
        }
    }
    buckets.extend(ownerless.iter().map(|&f| vec![f]));
    for cluster in raw {
        let mut free: Vec<i64> = cluster
            .iter()
            .copied()
            .filter(|f| active.contains(f) && !owners.contains_key(f) && !ownerless.contains(f))
            .collect();
        free.sort_unstable();
        if !free.is_empty() {
            buckets.push(free);
        }
    }
    buckets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Fixture {
        cases: Vec<Case>,
    }
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        priors: Vec<Prior>,
        clusters: Vec<Cluster>,
        fixed: Vec<Option<i64>>,
        preserved: HashSet<i64>,
        expected: Option<Vec<Option<i64>>>,
        reject: Option<bool>,
    }

    #[test]
    fn shared_fixtures() -> anyhow::Result<()> {
        let fixture: Fixture = serde_json::from_str(include_str!(
            "../../../../../../shared/test-corpus/stable-person-ids.json"
        ))?;
        for item in fixture.cases {
            let result = resolve(&item.priors, &item.clusters, &item.fixed, &item.preserved);
            if item.reject == Some(true) {
                assert!(result.is_err(), "{}", item.name);
            } else {
                assert_eq!(result?, item.expected.unwrap_or_default(), "{}", item.name);
            }
        }
        Ok(())
    }

    #[test]
    fn shared_protected_partitions() -> anyhow::Result<()> {
        #[derive(serde::Deserialize)]
        struct Owner {
            face: i64,
            person: i64,
        }
        #[derive(serde::Deserialize)]
        struct Item {
            name: String,
            raw: Vec<Vec<i64>>,
            owners: Vec<Owner>,
            different: Vec<(i64, i64)>,
            excluded: HashSet<i64>,
            expected: Vec<Vec<i64>>,
        }
        #[derive(serde::Deserialize)]
        struct Fixtures {
            cases: Vec<Item>,
        }
        let fixture: Fixtures = serde_json::from_str(include_str!(
            "../../../../../../shared/test-corpus/protected-face-partitions.json"
        ))?;
        for item in fixture.cases {
            let owners = item
                .owners
                .into_iter()
                .map(|o| (o.face, o.person))
                .collect();
            assert_eq!(
                partition(&item.raw, &owners, &item.different, &item.excluded),
                item.expected,
                "{}",
                item.name
            );
        }
        Ok(())
    }
}
