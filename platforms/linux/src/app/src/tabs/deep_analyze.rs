use crate::engine_client::{EngineClient, EngineState, QuerySpec};
use adw::prelude::*;
use std::{cell::RefCell, rc::Rc};

pub fn build_deep_analyze_tab(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let unavailable = adw::StatusPage::builder()
        .title("Deep Analyze unavailable on Linux")
        .icon_name("dialog-information-symbolic")
        .description("The Linux VLM runtime, model installation and inference are not supported by this engine. No captions, timings or smart names are generated here. Existing indexed captions may be reviewed below; they are not a new analysis result.")
        .build();
    body.append(&unavailable);
    let refresh = gtk::Button::with_label("Review indexed captions");
    body.append(&refresh);
    let results = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.append(&results);
    let runtime_status = gtk::Label::builder().xalign(0.0).wrap(true).build();
    body.append(&runtime_status);
    let rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                EngineState::DeepAnalyzeStarting(s) => runtime_status.set_label(&s.message),
                EngineState::DeepAnalyzeProgress(p) => runtime_status.set_label(&format!(
                    "Engine reported {} / {} analyzed",
                    p.processed, p.total
                )),
                EngineState::DeepAnalyzeFileDone(f) => runtime_status.set_label(&f.description),
                EngineState::DeepAnalyzeComplete(c) => runtime_status.set_label(&format!(
                    "Engine reported {} processed · {} failed",
                    c.processed, c.failed
                )),
                _ => {}
            }
        }
    });
    refresh.connect_clicked(move |_| {
        let rx = engine.borrow().query_files(QuerySpec {
            search: String::new(),
            kind: None,
            limit: 1000,
        });
        let results = results.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Some(child) = results.first_child() {
                results.remove(&child);
            }
            match rx.recv().await {
                Ok(Ok((files, _))) => {
                    let mut count = 0;
                    for file in files
                        .into_iter()
                        .filter(|f| f.description.is_some() || f.proposed_name.is_some())
                    {
                        count += 1;
                        results.append(
                            &gtk::Label::builder()
                                .xalign(0.0)
                                .wrap(true)
                                .selectable(true)
                                .label(format!(
                                    "{}\n{}\nIndexed proposed name: {}",
                                    file.path,
                                    file.description.as_deref().unwrap_or("No indexed caption"),
                                    file.proposed_name.as_deref().unwrap_or("None")
                                ))
                                .build(),
                        );
                    }
                    if count == 0 {
                        results.append(&gtk::Label::new(Some(
                            "No captions in the latest 1000 indexed files.",
                        )));
                    }
                }
                Ok(Err(e)) => {
                    results.append(&gtk::Label::new(Some(&format!("Library unavailable: {e}"))))
                }
                Err(e) => {
                    results.append(&gtk::Label::new(Some(&format!("Review interrupted: {e}"))))
                }
            }
        });
    });
    gtk::ScrolledWindow::builder()
        .child(&body)
        .hexpand(true)
        .vexpand(true)
        .build()
        .upcast()
}
