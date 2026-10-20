use std::path::Path;

use adw::prelude::*;
use fileid_engine::models::license::{self, LicensePolicy};
use gtk::glib;

/// Accepted on this device — in the shared `model-licenses.json`, or in the
/// per-policy marker file older GTK builds wrote, which is migrated once.
fn accepted_in(root: &Path, policy: &LicensePolicy) -> bool {
    if license::is_accepted(root, policy) {
        return true;
    }
    let legacy = root
        .join("license-acceptance")
        .join(format!("{}-{}.accepted", policy.key, policy.reviewed_at));
    if !std::fs::read_to_string(&legacy).is_ok_and(|value| value == "accepted\n") {
        return false;
    }
    if let Err(error) = license::record(root, policy) {
        tracing::warn!(policy = policy.key, ?error, "legacy license acceptance kept unmigrated");
        return true;
    }
    let _ = std::fs::remove_file(&legacy);
    true
}

// gtk::Dialog is deprecated in GTK 4.10+; this is the app's only use of it.
// Suppress the deprecation (which -D warnings turns into a build error) rather
// than rewrite to adw::AlertDialog blind — a proper migration is tracked in NEXT.
#[allow(deprecated)]
pub(crate) fn ensure_or_prompt(button: &gtk::Button, model_kind: &'static str) -> bool {
    let Some(policy) = license::policy_for(model_kind) else {
        return true;
    };
    let Ok(root) = fileid_engine::paths::root() else {
        tracing::warn!(
            policy = policy.key,
            "restricted model download refused: state root unavailable"
        );
        return false;
    };
    if accepted_in(&root, &policy) {
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
            match license::record(&root, &policy) {
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
    fn legacy_acceptance_markers_migrate_to_the_shared_store() {
        let root = std::env::temp_dir().join(format!(
            "fileid-linux-license-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let policy = license::policy_for("gemma_3_4b").unwrap();
        assert!(!accepted_in(&root, &policy));
        let legacy = root.join("license-acceptance").join("Gemma-2026-07-16.accepted");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "accepted\n").unwrap();
        assert!(accepted_in(&root, &policy));
        assert!(!legacy.exists(), "migrated once");
        assert!(license::is_accepted(&root, &policy));
        assert!(accepted_in(&root, &policy));
        std::fs::remove_dir_all(root).ok();
    }
}
