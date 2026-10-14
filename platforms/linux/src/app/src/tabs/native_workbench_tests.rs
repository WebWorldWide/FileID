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
    let tools = workbenches::tools(engine.clone());
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
    wait(|| button(&tools, "Preview").is_sensitive());

    entry(&catalog, "Search names, text, chapters and sampled frames").set_text("photo-fixture");
    button(&catalog, "Search").emit_clicked();
    wait(|| text(&catalog).contains(source.to_str().unwrap()));
    button(&catalog, "Chapters").emit_clicked();
    entry(&catalog, "Chapter title").set_text("Reviewed marker");
    button(&catalog, "Add / save chapter").emit_clicked();
    wait(|| text(&catalog).contains("Reviewed marker"));
    button(&catalog, "Delete").emit_clicked();
    wait(|| !text(&catalog).contains("Reviewed marker"));
    button(&catalog, "Undo chapter edit").emit_clicked();
    wait(|| text(&catalog).contains("Reviewed marker"));
    button(&catalog, "Edit").emit_clicked();
    entry(&catalog, "Chapter title").set_text("Changed marker");
    button(&catalog, "Add / save chapter").emit_clicked();
    wait(|| text(&catalog).contains("Changed marker"));
    button(&catalog, "Undo chapter edit").emit_clicked();
    wait(|| {
        text(&catalog).contains("Reviewed marker") && !text(&catalog).contains("Changed marker")
    });
    assert!(!descendants(&catalog)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .any(|b| matches!(b.label().as_deref(), Some("Pause" | "Resume" | "Retry" | "Cancel"))));
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
    button(&catalog, "Refresh jobs").emit_clicked();
    wait(|| text(&catalog).contains("catalogIndex · paused"));
    button(&catalog, "Cancel").emit_clicked();
    wait(|| {
        read.query_row(
            "SELECT state FROM catalog_jobs WHERE id='fixture-index-job'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
            == "cancelled"
    });
    button(&catalog, "Refresh jobs").emit_clicked();
    wait(|| text(&catalog).contains("catalogIndex · cancelled"));

    let search_mode = descendants(&catalog)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::DropDown>().ok())
        .next()
        .unwrap();
    search_mode.set_selected(1);
    button(&catalog, "Search").emit_clicked();
    wait(|| text(&catalog).contains("Using keyword search"));
    wait(|| text(&catalog).contains(source.to_str().unwrap()));

    stack.set_visible_child_name("tools");
    entry(&tools, "Catalog file IDs, comma separated (up to 100)").set_text("1");
    entry(&tools, "Existing absolute output folder").set_text(destination.to_str().unwrap());
    button(&tools, "Preview").emit_clicked();
    wait(|| button(&tools, "Execute preview").is_sensitive());
    assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
    let dimension = descendants(&tools)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::SpinButton>().ok())
        .next()
        .unwrap();
    dimension.set_value(1024.0);
    assert!(!button(&tools, "Execute preview").is_sensitive());
    button(&tools, "Preview").emit_clicked();
    wait(|| button(&tools, "Execute preview").is_sensitive());
    button(&tools, "Execute preview").emit_clicked();
    wait(|| text(&tools).contains("Export complete"));
    let photo_export = destination.join("photo-fixture - Export.png");
    assert!(photo_export.is_file());
    assert_eq!(std::fs::read(&source).unwrap(), png);
    button(&tools, "Last export").emit_clicked();
    wait(|| text(&tools).contains("Last export operation"));
    let photo_receipts = text(&tools);
    button(&tools, "Undo last export").emit_clicked();
    if workbenches::LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
        wait(|| text(&tools).contains("Exports moved to internal recovery storage"));
        assert!(!photo_export.exists());
        assert!(text(&tools).contains("Recoverable at"));
    } else {
        assert!(!button(&tools, "Undo last export").is_sensitive());
        assert!(text(&tools).contains("Export Undo unavailable"));
        assert!(photo_export.is_file());
        assert!(photo_receipts.contains(photo_export.to_str().unwrap()));
        assert!(text(&tools).contains(photo_export.to_str().unwrap()));
    }
    assert_eq!(std::fs::read(&source).unwrap(), png);
    let dropdowns = descendants(&tools)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::DropDown>().ok())
        .collect::<Vec<_>>();
    dropdowns[0].set_selected(1);
    dropdowns[1].set_selected(3);
    button(&tools, "Preview").emit_clicked();
    wait(|| button(&tools, "Execute preview").is_sensitive());
    button(&tools, "Execute preview").emit_clicked();
    wait(|| text(&tools).contains("Export complete"));
    let chapter_export = destination.join("photo-fixture - Chapters.json");
    assert!(std::fs::read_to_string(&chapter_export)
        .unwrap()
        .contains("Reviewed marker"));
    button(&tools, "Undo last export").emit_clicked();
    if workbenches::LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
        wait(|| text(&tools).contains("Exports moved to internal recovery storage"));
        assert!(!chapter_export.exists());
    } else {
        assert!(chapter_export.is_file());
        assert!(!button(&tools, "Undo last export").is_sensitive());
    }
    let preserved_files = count(&read, "files");

    stack.set_visible_child_name("chat");
    entry(&chat, "Ask about indexed files (maximum 2000 characters)").set_text("photo-fixture");
    button(&chat, "Send").emit_clicked();
    wait(|| text(&chat).contains("user: photo-fixture"));
    assert!(text(&chat).contains(source.to_str().unwrap()));
    assert_eq!(count(&read, "catalog_chat"), 2);
    button(&chat, "History").emit_clicked();
    wait(|| text(&chat).contains("Local conversation history"));
    assert!(text(&chat).contains("user: photo-fixture"));
    entry(&chat, "Conversation ID").set_text("other-conversation");
    button(&chat, "History").emit_clicked();
    wait(|| text(&chat).contains("Local conversation history"));
    assert!(!text(&chat).contains("user: photo-fixture"));
    entry(&chat, "Conversation ID").set_text("linux-default");
    button(&chat, "Clear conversation").emit_clicked();
    wait(|| text(&chat).contains("Conversation deleted"));
    assert_eq!(count(&read, "catalog_chat"), 0);
    assert_eq!(count(&read, "files"), preserved_files);
    button(&chat, "Stop").emit_clicked();
    wait(|| text(&chat).contains("No model response is running"));

    stack.set_visible_child_name("deep");
    assert!(text(&deep).contains("Deep Analyze"));
    stack.set_visible_child_name("restructure");
    assert!(text(&restructure).contains("Restructure"));
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
