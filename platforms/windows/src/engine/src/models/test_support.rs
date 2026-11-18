use std::path::Path;

pub(crate) fn receipt(root: &Path, value: serde_json::Value) {
    std::fs::write(root.join("receipt.json"), serde_json::to_vec(&value).unwrap()).unwrap();
}
