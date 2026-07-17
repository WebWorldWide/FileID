use std::io::Write;
use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::glib;

#[derive(Clone, Copy)]
struct LicensePolicy {
    key: &'static str,
    reviewed_at: &'static str,
    display_name: &'static str,
    terms_url: &'static str,
}

fn policy_for(model_kind: &str) -> Option<LicensePolicy> {
    match model_kind {
        "gemma_3_4b" | "gemma_3_12b" | "paligemma_3b" => Some(LicensePolicy {
            key: "Gemma",
            reviewed_at: "2026-07-16",
            display_name: "Google Gemma Terms of Use",
            terms_url: "https://ai.google.dev/gemma/terms",
        }),
        _ => None,
    }
}

fn marker_path(root: &Path, policy: LicensePolicy) -> PathBuf {
    root.join("license-acceptance")
        .join(format!("{}-{}.accepted", policy.key, policy.reviewed_at))
}

fn accepted_in(root: &Path, policy: LicensePolicy) -> bool {
    std::fs::read_to_string(marker_path(root, policy)).is_ok_and(|value| value == "accepted\n")
}

fn record_acceptance_in(root: &Path, policy: LicensePolicy) -> std::io::Result<()> {
    let directory = root.join("license-acceptance");
    std::fs::create_dir_all(&directory)?;
    let marker = marker_path(root, policy);
    let temporary = directory.join(format!(
        ".{}-{}-{}.tmp",
        policy.key,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(b"accepted\n")?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = std::fs::rename(&temporary, &marker) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn ensure_or_prompt(button: &gtk::Button, model_kind: &'static str) -> bool {
    let Some(policy) = policy_for(model_kind) else {
        return true;
    };
    let Ok(root) = fileid_engine::paths::root() else {
        tracing::warn!(
            policy = policy.key,
            "restricted model download refused: state root unavailable"
        );
        return false;
    };
    if accepted_in(&root, policy) {
        return true;
    }
    let Some(window) = button.root().and_downcast::<gtk::Window>() else {
        tracing::warn!(
            policy = policy.key,
            "restricted model download refused: no parent window"
        );
        return false;
    };

    let dialog = gtk::Dialog::builder()
        .title("License acceptance required")
        .transient_for(&window)
        .modal(true)
        .destroy_with_parent(true)
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("I Accept and Download", gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Cancel);

    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(
        &gtk::Label::builder()
            .label(format!(
                "This optional download is governed by the {}, not FileID's Apache-2.0 license. Review the terms before downloading. Acceptance is recorded only on this device.",
                policy.display_name
            ))
            .wrap(true)
            .xalign(0.0)
            .max_width_chars(64)
            .build(),
    );
    content.append(
        &gtk::LinkButton::builder()
            .label("Review full terms")
            .uri(policy.terms_url)
            .halign(gtk::Align::Start)
            .build(),
    );
    let error_label = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["error"])
        .build();
    content.append(&error_label);

    let button_weak = button.downgrade();
    dialog.connect_response(glib::clone!(
        #[weak]
        dialog,
        move |_, response| {
            if response != gtk::ResponseType::Accept {
                dialog.close();
                return;
            }
            match record_acceptance_in(&root, policy) {
                Ok(()) => {
                    dialog.close();
                    if let Some(button) = button_weak.upgrade() {
                        button.emit_clicked();
                    }
                }
                Err(error) => {
                    tracing::warn!(policy = policy.key, ?error, "restricted model download refused: acceptance persistence failed");
                    error_label.set_label("FileID couldn't save your acceptance, so the download was not started. Check the app data directory permissions and try again.");
                }
            }
        }
    ));
    dialog.present();
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_policy_is_versioned_and_persisted() {
        let root = std::env::temp_dir().join(format!(
            "fileid-linux-license-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let policy = policy_for("gemma_3_4b").unwrap();
        assert!(!accepted_in(&root, policy));
        record_acceptance_in(&root, policy).unwrap();
        assert!(accepted_in(&root, policy));
        assert!(marker_path(&root, policy)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("2026-07-16"));
        assert!(policy_for("qwen2_5_vl_7b").is_none());
        std::fs::remove_dir_all(root).ok();
    }
}
