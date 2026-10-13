pub const VLM_KIND: &str = "qwen2_5_vl_7b";

pub fn require_vlm() -> Result<(), String> {
    Err("Native AI analysis, timeline generation and model summaries are unavailable with this backend. Installed weights and optional executable configuration do not establish accepted loader or inference support. Stored captions and keyword retrieval remain available.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_runtime_capability_stays_closed_independent_of_configuration() {
        let reason = require_vlm().unwrap_err();
        assert!(reason.contains("unavailable with this backend"));
        assert!(reason.contains("Stored captions and keyword retrieval remain available"));
    }
}
