use std::path::Path;

pub(crate) fn receipt(root: &Path, value: serde_json::Value) {
    std::fs::write(root.join("receipt.json"), serde_json::to_vec(&value).unwrap()).unwrap();
}

pub(crate) fn listener() -> (std::net::TcpListener, String) {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}
