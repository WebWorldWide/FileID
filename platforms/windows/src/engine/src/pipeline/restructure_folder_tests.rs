use std::path::{Path, PathBuf};

use super::restructure::{classify_folders, Confidence, FolderClassification, ProposedMove};

fn moved(file_id: i64, folder: &str, category: &str) -> ProposedMove {
    ProposedMove {
        file_id,
        source: PathBuf::from(format!("{folder}/{file_id}.jpg")),
        destination: PathBuf::from(format!("/Library/{category}/{file_id}.jpg")),
        category: category.to_string(),
        confidence: Confidence::Auto,
        reason: None,
    }
}

fn class_of(moves: &[ProposedMove], folder: &str) -> FolderClassification {
    classify_folders(moves, Path::new("/Library"))
        .into_iter()
        .find(|c| c.source_folder == Path::new(folder))
        .unwrap()
        .classification
}

// Homogeneity is the share of the dominant destination category: one outlier
// among six files leaves the folder an Anchor (5/6 is above the 80% line), and
// an even split does not.
#[test]
fn a_folder_with_one_outlier_is_still_an_anchor_and_an_even_split_is_mixed() {
    let mut family: Vec<_> = (0..5).map(|i| moved(i, "/Library/Family", "Alice")).collect();
    family.push(moved(99, "/Library/Family", "Bob"));
    assert_eq!(class_of(&family, "/Library/Family"), FolderClassification::Anchor);

    let split: Vec<_> = (0..6).map(|i| moved(i, "/Library/Split", if i < 3 { "Alice" } else { "Bob" })).collect();
    assert_eq!(class_of(&split, "/Library/Split"), FolderClassification::Mixed);

    let four_of_six: Vec<_> = (0..6).map(|i| moved(i, "/Library/Four", if i < 4 { "Alice" } else { "Bob" })).collect();
    assert_eq!(class_of(&four_of_six, "/Library/Four"), FolderClassification::Mixed, "4/6 is below the 80% line");
}
