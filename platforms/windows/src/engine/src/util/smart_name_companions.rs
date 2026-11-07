//! Files that keep one name stem when smart names rename them: a RAW and its
//! JPEG/HEIC, a Live Photo's HEIC/JPEG and MOV, and `.xmp`/`.aae` sidecars
//! (`IMG_1.xmp` or darktable's `IMG_1.ARW.xmp`). Lockstep with macOS
//! `SmartNameCompanions.swift`.

use std::collections::HashSet;

use super::smart_name_style::{lower, split_name, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Image,
    Raw,
    Motion,
    Sidecar,
}

const RAW: &[&str] = &[
    "3fr", "arw", "cr2", "cr3", "dng", "erf", "iiq", "kdc", "mef", "mos", "mrw", "nef", "nrw", "orf", "pef",
    "raf", "raw", "rw2", "rwl", "srf", "srw", "x3f",
];

pub const UNSCANNED: &str = "unscannedCompanion";

pub fn role(extension: &str) -> Option<Role> {
    let extension = extension.to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" | "heic" | "heif" => Some(Role::Image),
        "mov" => Some(Role::Motion),
        "xmp" | "aae" => Some(Role::Sidecar),
        other if RAW.contains(&other) => Some(Role::Raw),
        _ => None,
    }
}

pub fn base(name: &str) -> Option<(String, String, Role)> {
    if name.starts_with('.') {
        return None;
    }
    let (mut stem, extension) = split_name(name);
    let role = role(&extension)?;
    if role == Role::Sidecar {
        let (inner, inner_extension) = split_name(&stem);
        if self::role(&inner_extension).is_some_and(|inner_role| inner_role != Role::Sidecar) {
            stem = inner;
        }
    }
    let suffix = name[stem.len()..].to_string();
    Some((stem, suffix, role))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub suffix: String,
    pub role: Role,
}

/// `name` and every companion of it in `listing`, in role then name order;
/// empty when it has none.
pub fn members(name: &str, listing: &[String]) -> Vec<Member> {
    let Some((stem, suffix, role)) = base(name) else { return Vec::new() };
    let key = lower(&stem);
    let mut found: Vec<Member> = listing
        .iter()
        .filter_map(|candidate| {
            let (other, suffix, role) = base(candidate)?;
            (lower(&other) == key).then(|| Member { name: candidate.clone(), suffix, role })
        })
        .collect();
    if !found.iter().any(|member| member.name == name) {
        found.push(Member { name: name.to_string(), suffix, role });
    }
    found.sort_by(|a, b| a.role.cmp(&b.role).then_with(|| a.name.cmp(&b.name)));
    found.dedup_by(|a, b| a.name == b.name);
    if is_group(&found) { found } else { Vec::new() }
}

pub fn is_group(members: &[Member]) -> bool {
    members.len() >= 2 && members.iter().any(|member| member.role != Role::Sidecar)
}

/// The first `_N` bump (1 = none, up to 99) at which every member's new name,
/// `stem` plus its own suffix, is free in the folder and the batch, or its own
/// name in another case. `members` holds each member's suffix and lowercased
/// current name.
pub fn shared_names(
    stem: &str,
    members: &[(String, String)],
    taken: &HashSet<String>,
    claimed: &HashSet<String>,
) -> Option<Vec<String>> {
    (1..=99).find_map(|bump| {
        let base = if bump == 1 { stem.to_string() } else { format!("{stem}_{bump}") };
        let names: Vec<String> = members.iter().map(|(suffix, _)| format!("{base}{suffix}")).collect();
        let mut keys = HashSet::new();
        let free = names.iter().zip(members).all(|(name, (_, own))| {
            let key = lower(name);
            keys.insert(key.clone()) && (key == *own || (!taken.contains(&key) && !claimed.contains(&key)))
        });
        free.then_some(names)
    })
}

/// Sidecars in `listing` that follow a rename of a `planned` RAW, JPEG/HEIC or
/// MOV: `<stem>.xmp`/`.aae` or `<name>.xmp` (extension in any case), not hidden
/// and not planned themselves. One is kept only when every same-stem RAW,
/// JPEG/HEIC or MOV in the folder is planned, and goes to the first planned
/// member with that stem as `(planned index, sidecar name, suffix after the stem)`.
pub fn following_sidecars(planned: &[String], listing: &[String]) -> Vec<(usize, String, String)> {
    let planned_names: HashSet<&str> = planned.iter().map(String::as_str).collect();
    let primary = |name: &str| base(name).filter(|(_, _, role)| *role != Role::Sidecar).map(|(stem, _, _)| stem);
    let mut taken: HashSet<&str> = HashSet::new();
    let mut found = Vec::new();
    for (index, name) in planned.iter().enumerate() {
        let Some(stem) = primary(name) else { continue };
        let key = lower(&stem);
        let complete = listing
            .iter()
            .all(|other| planned_names.contains(other.as_str()) || primary(other).is_none_or(|other| lower(&other) != key));
        if !complete {
            continue;
        }
        let owner = planned.iter().position(|other| primary(other).as_deref() == Some(stem.as_str())).unwrap_or(index);
        let named = |candidate: &str, prefix: &str, extensions: &[&str]| {
            candidate
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('.'))
                .is_some_and(|extension| extensions.iter().any(|allowed| extension.eq_ignore_ascii_case(allowed)))
        };
        for candidate in listing {
            if candidate.starts_with('.') || planned_names.contains(candidate.as_str()) || taken.contains(candidate.as_str()) {
                continue;
            }
            if named(candidate, &stem, &["xmp", "aae"]) || named(candidate, name, &["xmp"]) {
                taken.insert(candidate);
                found.push((owner, candidate.clone(), candidate[stem.len()..].to_string()));
            }
        }
    }
    found
}

/// Lower is better evidence, in the order `propose` reads its sources.
pub fn source_rank(source: &str, style: Style) -> u8 {
    match source {
        "chapter" => 0,
        "description" if style == Style::Descriptive => 1,
        "proposedName" if style != Style::Descriptive => 1,
        "description" | "proposedName" => 2,
        "tags" => 3,
        "event" => 4,
        _ => 5,
    }
}

pub fn group_reason(reason: &str, leader: i64) -> String {
    format!("{reason}|group:{leader}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn names(members: &[Member]) -> Vec<&str> {
        members.iter().map(|member| member.name.as_str()).collect()
    }

    #[test]
    fn groups_raw_jpeg_live_photos_and_sidecars() {
        let folder = listing(&[
            "DSC_0012.ARW", "DSC_0012.JPG", "DSC_0012.xmp", "DSC_0012.ARW.xmp", ".DSC_0012.JPG.fileid-tags.json",
            "DSC_0012.txt", "IMG_0001.HEIC", "IMG_0001.MOV", "IMG_0001.AAE", "DSC_0013.ARW", "notes.xmp", "report.pdf",
            "report.docx",
        ]);
        let pair = members("DSC_0012.JPG", &folder);
        assert_eq!(names(&pair), ["DSC_0012.JPG", "DSC_0012.ARW", "DSC_0012.ARW.xmp", "DSC_0012.xmp"]);
        assert_eq!(pair[2].suffix, ".ARW.xmp");
        assert_eq!(names(&members("DSC_0012.xmp", &folder)), names(&pair));
        assert_eq!(names(&members("IMG_0001.MOV", &folder)), ["IMG_0001.HEIC", "IMG_0001.MOV", "IMG_0001.AAE"]);
        assert!(members("DSC_0013.ARW", &folder).is_empty());
        assert!(members("notes.xmp", &listing(&["notes.xmp", "notes.aae"])).is_empty());
        assert!(members("report.pdf", &folder).is_empty());
        assert_eq!(names(&members("a.jpg", &listing(&["A.CR3"]))), ["a.jpg", "A.CR3"]);
        assert_eq!(role("\u{212A}DC"), None);
    }

    #[test]
    fn shared_names_bump_together_or_not_at_all() {
        let members = vec![(".JPG".to_string(), "dsc_1.jpg".to_string()), (".ARW".to_string(), "dsc_1.arw".to_string())];
        let none = HashSet::new();
        assert_eq!(shared_names("beach", &members, &none, &none), Some(vec!["beach.JPG".into(), "beach.ARW".into()]));
        let taken: HashSet<String> = ["beach.arw".to_string()].into();
        assert_eq!(shared_names("beach", &members, &taken, &none), Some(vec!["beach_2.JPG".into(), "beach_2.ARW".into()]));
        let full: HashSet<String> =
            (1..=99).map(|bump| if bump == 1 { "beach.arw".into() } else { format!("beach_{bump}.arw") }).collect();
        assert_eq!(shared_names("beach", &members, &full, &none), None);
        let twins = vec![(".JPG".to_string(), "a.jpg".to_string()), (".jpg".to_string(), "a.jpg".to_string())];
        assert_eq!(shared_names("beach", &twins, &none, &none), None);
    }

    #[test]
    fn sidecars_follow_only_a_complete_group() {
        let folder = listing(&[
            "IMG_1.JPG", "IMG_1.ARW", "IMG_1.xmp", "IMG_1.ARW.XMP", "IMG_1.aae", ".IMG_1.xmp", "IMG_1.txt.xmp", "img_1.jpg.xmp",
            "IMG_2.HEIC", "IMG_2.AAE", "IMG_3.JPG", "IMG_3.xmp",
        ]);
        let pair = listing(&["IMG_1.JPG", "IMG_1.ARW", "IMG_2.HEIC"]);
        assert_eq!(
            following_sidecars(&pair, &folder),
            [
                (0, "IMG_1.xmp".to_string(), ".xmp".to_string()),
                (0, "IMG_1.aae".to_string(), ".aae".to_string()),
                (0, "IMG_1.ARW.XMP".to_string(), ".ARW.XMP".to_string()),
                (2, "IMG_2.AAE".to_string(), ".AAE".to_string()),
            ]
        );
        assert!(following_sidecars(&listing(&["IMG_1.JPG"]), &folder).is_empty());
        assert_eq!(following_sidecars(&listing(&["IMG_3.JPG"]), &folder), [(0, "IMG_3.xmp".to_string(), ".xmp".to_string())]);
        assert!(following_sidecars(&listing(&["IMG_3.JPG", "IMG_3.xmp"]), &folder).is_empty());
        assert!(following_sidecars(&listing(&["IMG_3.xmp"]), &folder).is_empty());
    }

    #[test]
    fn evidence_rank_follows_the_style() {
        assert!(source_rank("chapter", Style::Short) < source_rank("proposedName", Style::Short));
        assert!(source_rank("proposedName", Style::Short) < source_rank("description", Style::Short));
        assert!(source_rank("description", Style::Descriptive) < source_rank("proposedName", Style::Descriptive));
        assert!(source_rank("event", Style::DateFirst) < source_rank("noEvidence", Style::DateFirst));
        assert_eq!(group_reason("tags", 7), "tags|group:7");
    }
}
