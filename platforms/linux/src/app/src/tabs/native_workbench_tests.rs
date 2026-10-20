use super::{deep_analyze, restructure, workbenches};
use crate::engine_client::EngineClient;
use adw::prelude::*;
use std::{
    cell::RefCell,
    path::Path,
    rc::Rc,
    time::{Duration, Instant},
};

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut widgets = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(widget) = child {
        widgets.extend(descendants(&widget));
        child = widget.next_sibling();
    }
    widgets
}
fn button(root: &gtk::Widget, text: &str) -> gtk::Button {
    descendants(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.label().as_deref() == Some(text))
        .unwrap_or_else(|| panic!("Missing button: {text}"))
}
fn tooltip_button(root: &gtk::Widget, tooltip: &str) -> gtk::Button {
    descendants(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.tooltip_text().as_deref() == Some(tooltip))
        .unwrap_or_else(|| panic!("Missing button with tooltip: {tooltip}"))
}
fn first_with_class(root: &gtk::Widget, class: &str) -> gtk::Button {
    descendants(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.has_css_class(class))
        .unwrap_or_else(|| panic!("Missing button with class: {class}"))
}
fn in_job_row(widget: &gtk::Widget) -> bool {
    let mut parent = widget.parent();
    while let Some(ancestor) = parent {
        if ancestor.has_css_class("v3-job-row") {
            return true;
        }
        parent = ancestor.parent();
    }
    false
}
fn job_buttons(root: &gtk::Widget) -> Vec<gtk::Button> {
    descendants(root)
        .into_iter()
        .filter(in_job_row)
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .collect()
}
fn entry(root: &gtk::Widget, hint: &str) -> gtk::Entry {
    descendants(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Entry>().ok())
        .find(|e| e.placeholder_text().as_deref() == Some(hint))
        .unwrap_or_else(|| panic!("Missing entry: {hint}"))
}
fn text(root: &gtk::Widget) -> String {
    descendants(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.text().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
#[track_caller]
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let context = glib::MainContext::default();
    loop {
        while context.pending() {
            context.iteration(false);
        }
        if predicate() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for native GTK/engine response"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn count(conn: &rusqlite::Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
#[ignore = "requires Xvfb/display and FILEID_ENGINE pointing to an offline built real engine; run alone"]
fn native_workbenches_round_trip_real_engine_in_disposable_xdg_state() {
    assert!(Path::new(&std::env::var("FILEID_ENGINE").expect("FILEID_ENGINE required")).is_file());
    adw::init().unwrap();
    workbenches::run_review_regressions();
    let fixture = std::env::temp_dir().join(super::workbench_state::request_id());
    let old_xdg = std::env::var_os("XDG_DATA_HOME");
    std::env::set_var("XDG_DATA_HOME", fixture.join("state"));
    fileid_engine::paths::ensure_state_dirs().unwrap();
    let library = fixture.join("library");
    let destination = fixture.join("exports");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::create_dir_all(&destination).unwrap();
    let source = library.join("photo-fixture.png");
    let png = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 6,
        0, 0, 0, 114, 182, 13, 36, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 127, 134, 225,
        63, 8, 51, 192, 24, 0, 102, 18, 11, 41, 185, 120, 131, 105, 0, 0, 0, 0, 73, 69, 78, 68,
        174, 66, 96, 130,
    ];
    std::fs::write(&source, png).unwrap();
    let db_path = fileid_engine::paths::db_path().unwrap();
    {
        let conn = fileid_engine::db::open_writer(&db_path).unwrap();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,vlm_description) VALUES(1,?1,1,?2,0,'image','png','Previously indexed gold square fixture caption')",rusqlite::params![source.to_str().unwrap(),png.len()]).unwrap();
        conn.execute("INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,progress,error,created_at,updated_at) VALUES('fixture-job','timelineSample','[1]','{}','completed',1,'Synthetic completed record; no timeline inference ran',0,0)",[]).unwrap();
    }
    let engine = Rc::new(RefCell::new(EngineClient::new()));
    let catalog = workbenches::catalog(engine.clone());
    let (tools, tools_ui) = super::file_tools::build_tools(engine.clone());
    let chat = workbenches::chat(engine.clone());
    let deep = deep_analyze::build_deep_analyze_tab(engine.clone());
    let restructure = restructure::build_restructure_tab(engine.clone());
    let stack = gtk::Stack::new();
    for (name, widget) in [
        ("catalog", &catalog),
        ("tools", &tools),
        ("chat", &chat),
        ("deep", &deep),
        ("restructure", &restructure),
    ] {
        stack.add_named(widget, Some(name));
    }
    let window = adw::Window::builder()
        .default_width(1000)
        .default_height(700)
        .content(&stack)
        .build();
    window.present();
    EngineClient::start(&engine);
    wait(|| engine.borrow().is_ready());
    wait(|| !tools_ui.state.borrow().capabilities.is_empty());

    let query = entry(&catalog, "Search names, captions, chapters");
    query.set_text("photo-fixture");
    query.emit_activate();
    wait(|| text(&catalog).contains(source.to_str().unwrap()));
    first_with_class(&catalog, "v3-rail").emit_clicked();
    button(&catalog, "+ Add chapter").emit_clicked();
    entry(&catalog, "Chapter title").set_text("Reviewed marker");
    button(&catalog, "Add chapter").emit_clicked();
    wait(|| text(&catalog).contains("Reviewed marker"));
    tooltip_button(&catalog, "Delete chapter").emit_clicked();
    wait(|| !text(&catalog).contains("Reviewed marker"));
    button(&catalog, "Undo last edit").emit_clicked();
    wait(|| text(&catalog).contains("Reviewed marker"));
    button(&catalog, "Rename").emit_clicked();
    entry(&catalog, "Chapter title").set_text("Changed marker");
    button(&catalog, "Save chapter").emit_clicked();
    wait(|| text(&catalog).contains("Changed marker"));
    button(&catalog, "Undo last edit").emit_clicked();
    wait(|| {
        text(&catalog).contains("Reviewed marker") && !text(&catalog).contains("Changed marker")
    });
    assert!(!job_buttons(&catalog)
        .into_iter()
        .any(|b| matches!(
            b.label().as_deref(),
            Some("Pause" | "Resume" | "Retry" | "Cancel")
        )));
    let read = fileid_engine::db::open_read(&db_path).unwrap();
    assert_eq!(
        read.query_row(
            "SELECT state FROM catalog_jobs WHERE id='fixture-job'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
        "completed"
    );
    {
        let conn = fileid_engine::db::open_writer(&db_path).unwrap();
        conn.execute("INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,progress,error,created_at,updated_at) VALUES('fixture-index-job','catalogIndex','[1]','{}','paused',0,'Synthetic paused index; no model inference ran',0,0)",[]).unwrap();
    }
    wait(|| text(&catalog).contains("Catalog index") && text(&catalog).contains("Paused"));
    job_buttons(&catalog)
        .into_iter()
        .find(|b| b.label().as_deref() == Some("Cancel"))
        .expect("paused index job offers Cancel")
        .emit_clicked();
    wait(|| {
        read.query_row(
            "SELECT state FROM catalog_jobs WHERE id='fixture-index-job'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
            == "cancelled"
    });
    wait(|| text(&catalog).contains("Cancelled"));

    button(&catalog, "Hybrid").emit_clicked();
    query.emit_activate();
    wait(|| text(&catalog).contains("Using keyword search"));
    wait(|| text(&catalog).contains(source.to_str().unwrap()));

    stack.set_visible_child_name("tools");
    tools_ui.use_destination(destination.to_str().unwrap());
    let first_check = |root: &gtk::Widget| {
        descendants(root).into_iter().find_map(|w| w.downcast::<gtk::CheckButton>().ok())
    };
    wait(|| first_check(&tools).is_some());
    first_check(&tools).unwrap().set_active(true);
    wait(|| button(&tools, "Preview").is_sensitive());
    button(&tools, "Preview").emit_clicked();
    wait(|| button(&tools, "Export").is_sensitive());
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
    let dimension = descendants(&tools)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::SpinButton>().ok())
        .next()
        .unwrap();
    dimension.set_value(1024.0);
    assert!(!button(&tools, "Export").is_sensitive());
    button(&tools, "Preview").emit_clicked();
    wait(|| button(&tools, "Export").is_sensitive());
    button(&tools, "Export").emit_clicked();
    wait(|| text(&tools).contains("Export complete"));
    let photo_export = destination.join("photo-fixture - Export.png");
    assert!(photo_export.is_file());
    assert_eq!(std::fs::read(&source).unwrap(), png);
    button(&tools, "Show last export").emit_clicked();
    wait(|| text(&tools).contains("photo-fixture - Export.png"));
    button(&tools, "Undo").emit_clicked();
    if workbenches::LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
        wait(|| text(&tools).contains("Exports moved to internal recovery storage"));
        assert!(!photo_export.exists());
    } else {
        assert!(!button(&tools, "Undo").is_sensitive());
        assert!(photo_export.is_file());
    }
    assert_eq!(std::fs::read(&source).unwrap(), png);
    button(&tools, "Chapters").emit_clicked();
    button(&tools, "JSON").emit_clicked();
    wait(|| first_check(&tools).is_some());
    first_check(&tools).unwrap().set_active(true);
    wait(|| button(&tools, "Preview").is_sensitive());
    button(&tools, "Preview").emit_clicked();
    wait(|| button(&tools, "Export").is_sensitive());
    button(&tools, "Export").emit_clicked();
    wait(|| text(&tools).contains("Export complete"));
    let chapter_export = destination.join("photo-fixture - Chapters.json");
    assert!(std::fs::read_to_string(&chapter_export)
        .unwrap()
        .contains("Reviewed marker"));
    button(&tools, "Undo").emit_clicked();
    if workbenches::LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
        wait(|| text(&tools).contains("Exports moved to internal recovery storage"));
        assert!(!chapter_export.exists());
    } else {
        assert!(chapter_export.is_file());
        assert!(!button(&tools, "Undo").is_sensitive());
    }
    let preserved_files = count(&read, "files");

    stack.set_visible_child_name("chat");
    chat.activate_action("chat.ask", Some(&"photo-fixture".to_variant())).unwrap();
    button(&chat, "Send").emit_clicked();
    wait(|| text(&chat).contains("photo-fixture") && text(&chat).contains("You"));
    wait(|| text(&chat).contains("photo-fixture.png"));
    assert_eq!(count(&read, "catalog_chat"), 2);
    button(&chat, "Clear").emit_clicked();
    wait(|| text(&chat).contains("Conversation deleted"));
    assert_eq!(count(&read, "catalog_chat"), 0);
    assert_eq!(count(&read, "files"), preserved_files);
    button(&chat, "Stop").emit_clicked();
    wait(|| text(&chat).contains("No model response is running"));

    stack.set_visible_child_name("deep");
    assert!(text(&deep).contains("Deep Analyze"));
    stack.set_visible_child_name("restructure");
    assert!(text(&restructure).contains("Preview the proposed moves"));
    assert!(source.is_file());
    assert_eq!(count(&read, "files"), preserved_files);
    engine
        .borrow_mut()
        .send(fileid_engine::ipc::CommandPayload::Shutdown(
            fileid_engine::ipc::Empty {},
        ))
        .unwrap();
    wait(|| !engine.borrow().is_ready());
    drop(read);
    window.close();
    if let Some(value) = old_xdg {
        std::env::set_var("XDG_DATA_HOME", value);
    } else {
        std::env::remove_var("XDG_DATA_HOME");
    }
    std::fs::remove_dir_all(fixture).unwrap();
}
