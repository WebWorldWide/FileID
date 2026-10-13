use super::*;
use std::time::Instant;

fn pump_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        if ready() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Controlled GTK fixture response was not consumed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn buttons(root: &gtk::Widget) -> Vec<gtk::Button> {
    let mut result = vec![];
    if let Some(button) = root.downcast_ref::<gtk::Button>() {
        result.push(button.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        result.extend(buttons(&widget));
        child = widget.next_sibling();
    }
    result
}
fn hit(id: i64) -> CatalogHit {
    CatalogHit {
        file_id: id,
        path: format!("/fixture/file-{id}"),
        kind: "video".into(),
        text: String::new(),
        evidence_id: None,
        start_seconds: None,
        page: None,
    }
}
fn chapter() -> CatalogChapter {
    CatalogChapter {
        id: "chapter-a".into(),
        file_id: 1,
        start_seconds: 0.0,
        end_seconds: 2.0,
        title: "A chapter".into(),
        summary: String::new(),
        source_revision: "fixture".into(),
        model_version: "user".into(),
        confidence: 1.0,
        user_edited: true,
        stale: false,
    }
}

fn catalog_selection_clears_chapters_and_rejects_stale_controls_through_failed_b_detail() {
    let engine = Rc::new(RefCell::new(EngineClient::new()));
    let (_widget, ui) = build_catalog(engine.clone());
    ui.select(hit(1));
    ui.render_chapters(vec![chapter()]);
    let controls = buttons(&ui.chapters.clone().upcast());
    let edit = controls
        .iter()
        .find(|b| b.label().as_deref() == Some("Edit"))
        .unwrap();
    let delete = controls
        .iter()
        .find(|b| b.label().as_deref() == Some("Delete"))
        .unwrap();
    ui.select(hit(2));
    assert!(ui.chapters.first_child().is_none());
    ui.requests.borrow_mut().begin("jobs", "unrelated-jobs");
    ui.requests.borrow_mut().begin("detail", "b-detail");
    edit.emit_clicked();
    delete.emit_clicked();
    assert!(ui.editing.borrow().is_none());
    assert!(ui.title.text().is_empty());
    assert!(ui.requests.borrow().busy("jobs"));
    assert!(ui.requests.borrow().busy("detail"));
    engine
        .borrow()
        .publish_test_event(EngineState::CatalogResponse(CatalogResponse {
            request_id: "a-detail".into(),
            status: "ok".into(),
            message: None,
            hits: vec![],
            chapters: vec![chapter()],
            jobs: vec![],
        }));
    engine
        .borrow()
        .publish_test_event(EngineState::CatalogResponse(CatalogResponse {
            request_id: "b-detail".into(),
            status: "error".into(),
            message: Some("B detail failed (fixture)".into()),
            hits: vec![],
            chapters: vec![],
            jobs: vec![],
        }));
    pump_until(|| ui.status.text() == "B detail failed (fixture)");
    assert!(ui.chapters.first_child().is_none());
    edit.emit_clicked();
    delete.emit_clicked();
    assert!(ui.editing.borrow().is_none());
    assert!(ui.title.text().is_empty());
    assert_eq!(ui.status.text(), "B detail failed (fixture)");
    assert!(ui.requests.borrow().busy("jobs"));
}

fn tools_receipts_stay_bound_to_preview_and_undo_remains_guarded() {
    let engine = Rc::new(RefCell::new(EngineClient::new()));
    engine.borrow().publish_test_event(EngineState::Ready);
    let (_widget, ui) = build_tools(engine.clone());
    let capability = ToolCapability {
        id: "photo".into(),
        available: true,
        input_formats: vec!["png".into()],
        output_formats: vec!["png".into()],
        detail: "Fixture capability".into(),
    };
    ui.state.borrow_mut().capabilities = vec![capability.clone()];
    ui.state
        .borrow_mut()
        .requests
        .begin("operation", "preview-a");
    ui.state.borrow_mut().operation_action = Some("preview".into());
    let receipt = |id, path: &str, state: &str| ToolOutput {
        file_id: id,
        source_path: "/fixture/original".into(),
        output_path: path.into(),
        state: state.into(),
        message: "Fixture receipt".into(),
    };
    engine
        .borrow()
        .publish_test_event(EngineState::ToolResponse(ToolResponse {
            request_id: "preview-a".into(),
            status: "ok".into(),
            message: "Preview A".into(),
            operation_id: Some("operation-a".into()),
            outputs: vec![receipt(1, "/fixture/A.png", "pending")],
            capabilities: vec![],
        }));
    pump_until(|| ui.state.borrow().preview.as_deref() == Some("operation-a"));
    let old_rows = ui.outputs.first_child().unwrap();
    assert!(ui.execute.is_sensitive());
    ui.state.borrow_mut().requests.begin("capabilities", "caps");
    engine
        .borrow()
        .publish_test_event(EngineState::ToolResponse(ToolResponse {
            request_id: "caps".into(),
            status: "ok".into(),
            message: "Capabilities (fixture)".into(),
            operation_id: None,
            outputs: vec![],
            capabilities: vec![capability],
        }));
    pump_until(|| ui.status.text().contains("Capabilities (fixture)"));
    assert_eq!(ui.outputs.first_child().unwrap(), old_rows);
    assert_eq!(ui.state.borrow().preview.as_deref(), Some("operation-a"));
    assert!(ui.execute.is_sensitive());
    ui.state.borrow_mut().requests.begin("history", "history-b");
    engine
        .borrow()
        .publish_test_event(EngineState::ToolResponse(ToolResponse {
            request_id: "history-b".into(),
            status: "ok".into(),
            message: "History B".into(),
            operation_id: Some("operation-b".into()),
            outputs: vec![
                receipt(1, "/fixture/B1.png", "completed"),
                receipt(2, "/fixture/B2.png", "completed"),
            ],
            capabilities: vec![],
        }));
    pump_until(|| ui.state.borrow().history.as_deref() == Some("operation-b"));
    assert!(ui.state.borrow().preview.is_none());
    assert!(!ui.execute.is_sensitive());
    let history_rows = ui.outputs.first_child().unwrap();
    let mut children = 0;
    let mut child = ui.outputs.first_child();
    while let Some(widget) = child {
        children += 1;
        child = widget.next_sibling();
    }
    assert_eq!(children, 3);
    ui.state
        .borrow_mut()
        .requests
        .begin("history", "history-error");
    engine
        .borrow()
        .publish_test_event(EngineState::ToolResponse(ToolResponse {
            request_id: "history-error".into(),
            status: "error".into(),
            message: "History read failed (fixture)".into(),
            operation_id: None,
            outputs: vec![],
            capabilities: vec![],
        }));
    pump_until(|| ui.status.text().contains("History read failed (fixture)"));
    assert_eq!(ui.outputs.first_child().unwrap(), history_rows);
    assert_eq!(ui.state.borrow().history.as_deref(), Some("operation-b"));
    ui.execute.emit_clicked();
    assert_eq!(ui.outputs.first_child().unwrap(), history_rows);
    if !LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
        assert!(!ui.undo.is_sensitive());
        ui.state
            .borrow_mut()
            .requests
            .begin("capabilities", "unrelated-caps");
        ui.undo.emit_clicked();
        assert_eq!(ui.status.text(), TOOL_UNDO_UNAVAILABLE_REASON);
        assert!(ui.state.borrow().requests.busy("capabilities"));
        assert!(!ui.state.borrow().requests.busy("operation"));
        assert_eq!(ui.state.borrow().history.as_deref(), Some("operation-b"));
        assert_eq!(ui.outputs.first_child().unwrap(), history_rows);
    }
}

fn tools_cancel_ack_keeps_execute_owned_until_terminal_and_ignores_late_ack() {
    let engine = Rc::new(RefCell::new(EngineClient::new()));
    engine.borrow().publish_test_event(EngineState::Ready);
    let (_widget, ui) = build_tools(engine.clone());
    {
        let mut state = ui.state.borrow_mut();
        state.capabilities.push(ToolCapability {
            id: "cancel".into(),
            available: true,
            input_formats: vec![],
            output_formats: vec![],
            detail: "Fixture interruptible export".into(),
        });
        state.requests.begin("operation", "execute-a");
        state.operation_action = Some("execute".into());
        state.active_operation = Some("operation-a".into());
        assert!(state.can_cancel());
        state.requests.begin("cancel", "cancel-a");
        state.cancelling = true;
    }
    let response = |id: &str, status: &str, message: &str| {
        EngineState::ToolResponse(ToolResponse {
            request_id: id.into(),
            status: status.into(),
            message: message.into(),
            operation_id: Some("operation-a".into()),
            outputs: vec![],
            capabilities: vec![],
        })
    };
    engine
        .borrow()
        .publish_test_event(response("cancel-a", "ok", "Fixture: Stopping…"));
    pump_until(|| ui.status.text().contains("Stopping"));
    assert!(ui.state.borrow().requests.busy("operation"));
    assert_eq!(
        ui.state.borrow().active_operation.as_deref(),
        Some("operation-a")
    );
    assert!(ui.state.borrow().cancelling);
    assert!(!ui.execute.is_sensitive());
    engine
        .borrow()
        .publish_test_event(response("execute-a", "error", "Fixture: export cancelled"));
    pump_until(|| ui.status.text().contains("export cancelled"));
    assert!(!ui.state.borrow().requests.busy("operation"));
    assert!(!ui.state.borrow().cancelling);
    engine
        .borrow()
        .publish_test_event(response("cancel-a", "ok", "Late acknowledgement"));
    while glib::MainContext::default().pending() {
        glib::MainContext::default().iteration(false);
    }
    assert!(!ui.status.text().contains("Late"));
}

fn widget_text(widget: &gtk::Widget) -> String {
    let mut text = widget
        .downcast_ref::<gtk::Label>()
        .map(|l| l.text().to_string())
        .unwrap_or_default();
    let mut child = widget.first_child();
    while let Some(w) = child {
        text.push_str(&widget_text(&w));
        child = w.next_sibling();
    }
    text
}

fn chat_streams_owned_model_request_and_stop_hides_late_stream_without_releasing_write() {
    let engine = Rc::new(RefCell::new(EngineClient::new()));
    let (widget, state) = build_chat(engine.clone());
    let send = ChatRequest {
        request_id: "model-fixture".into(),
        conversation_id: "linux-default".into(),
        action: "send".into(),
        text: Some("fixture".into()),
        use_model: Some(true),
    };
    assert!(state.borrow_mut().begin(&send).unwrap());
    let response = |id: &str, conversation: &str, status: &str, text: &str| {
        EngineState::ChatResponse(ChatResponse {
            request_id: id.into(),
            conversation_id: conversation.into(),
            status: status.into(),
            message: "Controlled model fixture, no model executed".into(),
            messages: vec![ChatMessage {
                id: "assistant-fixture".into(),
                role: "assistant".into(),
                text: text.into(),
                created_at: 0.0,
            }],
            hits: vec![],
        })
    };
    engine.borrow().publish_test_event(response(
        "model-fixture",
        "other",
        "streaming",
        "Wrong conversation",
    ));
    engine.borrow().publish_test_event(response(
        "model-fixture",
        "linux-default",
        "streaming",
        "Fixture partial caption",
    ));
    pump_until(|| widget_text(&widget).contains("Fixture partial caption"));
    assert!(!widget_text(&widget).contains("Wrong conversation"));
    let cancel = ChatRequest {
        request_id: "stop-fixture".into(),
        action: "cancel".into(),
        text: None,
        ..send.clone()
    };
    assert!(state.borrow_mut().begin(&cancel).unwrap());
    engine.borrow().publish_test_event(response(
        "stop-fixture",
        "linux-default",
        "cancelled",
        "Fixture stopped",
    ));
    pump_until(|| widget_text(&widget).contains("Fixture stopped"));
    engine.borrow().publish_test_event(response(
        "model-fixture",
        "linux-default",
        "streaming",
        "Late partial",
    ));
    while glib::MainContext::default().pending() {
        glib::MainContext::default().iteration(false);
    }
    assert!(!widget_text(&widget).contains("Late partial"));
    assert!(state
        .borrow_mut()
        .begin(&ChatRequest {
            request_id: "overlap-fixture".into(),
            ..send.clone()
        })
        .is_err());
    engine.borrow().publish_test_event(response(
        "model-fixture",
        "linux-default",
        "completed",
        "Late terminal",
    ));
    pump_until(|| {
        state
            .borrow_mut()
            .begin(&ChatRequest {
                request_id: "next-fixture".into(),
                ..send.clone()
            })
            .is_ok()
    });
    assert!(!widget_text(&widget).contains("Late terminal"));
}

fn catalog_detail_evidence_is_selection_owned_and_never_claims_complete_coverage() {
    let engine = Rc::new(RefCell::new(EngineClient::new()));
    let (_widget, ui) = build_catalog(engine.clone());
    *ui.selected.borrow_mut() = Some(hit(1));
    ui.requests.borrow_mut().begin("detail", "detail-a");
    let mut selected = hit(1);
    selected.kind = "sampledFrame".into();
    selected.text = "Fixture sparse frame".into();
    selected.start_seconds = Some(10.0);
    let mut other = hit(2);
    other.text = "Wrong selected video".into();
    engine
        .borrow()
        .publish_test_event(EngineState::CatalogResponse(CatalogResponse {
            request_id: "detail-a".into(),
            status: "ok".into(),
            message: None,
            chapters: vec![],
            jobs: vec![],
            hits: vec![selected, other],
        }));
    pump_until(|| widget_text(&ui.detail_hits.clone().upcast()).contains("Fixture sparse frame"));
    assert!(widget_text(&ui.detail_hits.clone().upcast()).contains("sampled frame, unverified"));
    assert!(!widget_text(&ui.detail_hits.clone().upcast()).contains("Wrong selected video"));
    ui.requests.borrow_mut().begin("detail", "detail-empty");
    engine
        .borrow()
        .publish_test_event(EngineState::CatalogResponse(CatalogResponse {
            request_id: "detail-empty".into(),
            status: "ok".into(),
            message: None,
            chapters: vec![],
            jobs: vec![],
            hits: vec![],
        }));
    pump_until(|| {
        widget_text(&ui.detail_hits.clone().upcast()).contains("Coverage is unknown, not complete")
    });
}

pub(super) fn run() {
    catalog_detail_evidence_is_selection_owned_and_never_claims_complete_coverage();
    chat_streams_owned_model_request_and_stop_hides_late_stream_without_releasing_write();
    tools_cancel_ack_keeps_execute_owned_until_terminal_and_ignores_late_ack();
    catalog_selection_clears_chapters_and_rejects_stale_controls_through_failed_b_detail();
    tools_receipts_stay_bound_to_preview_and_undo_remains_guarded();
}
