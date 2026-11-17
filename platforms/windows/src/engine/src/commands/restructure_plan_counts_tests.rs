use super::restructure::handle_plan_restructure;
use crate::ipc::{self, sink::Sink, EventPayload};

// The preview's auto / review / ask counts are the tally of the moves it lists,
// and together they add up to the number of moves in the plan.
#[tokio::test]
async fn the_plan_publishes_confidence_counts_that_match_its_moves_and_sum_to_the_total() {
    let root = std::env::temp_dir().join(format!("fileid-plan-counts-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let conn = crate::db::test_support::memory_db();
    let sep19 = 1_789_840_800.0;
    let mar1 = 1_772_388_000.0;
    let mut rows: Vec<(String, f64)> =
        (0..5).map(|i| (format!("DCIM/100MSDCF/DSC0000{i}.JPG"), sep19 + f64::from(i) * 600.0)).collect();
    rows.extend((0..3).map(|i| (format!("Event/Photos/JPEGs/D{i}.ARW"), mar1 + f64::from(i) * 60.0)));
    rows.extend((0..2).map(|i| (format!("Event/Photos/JPEGs/D{i}.JPG"), mar1 + f64::from(i) * 60.0)));
    rows.extend((0..2).map(|i| (format!("Event/Photos/RAWs/R{i}.ARW"), mar1 + f64::from(i) * 60.0)));
    rows.extend((0..4).map(|i| (format!("Downloads/doc{i}.pdf"), mar1 + f64::from(i) * 60.0)));
    for (index, (relative, time)) in rows.iter().enumerate() {
        let path = root.join(relative);
        let extension = path.extension().unwrap().to_string_lossy().to_lowercase();
        let kind = if extension == "pdf" { "pdf" } else { "image" };
        conn.execute(
            "INSERT INTO files(id, path_text, path_hash, size_bytes, created_at, modified_at, scanned_at, kind, extension)
             VALUES (?1, ?2, ?1, 1000, ?3, ?3, ?3, ?5, ?4)",
            rusqlite::params![index as i64 + 1, path.to_string_lossy(), time, extension, kind],
        )
        .unwrap();
    }
    let db = std::sync::Arc::new(parking_lot::Mutex::new(conn));
    let (sink, mut events) = Sink::channel_for_test(8);
    let payload = ipc::PlanRestructurePayload { library_root: root.to_string_lossy().into_owned(), supports_paged_plans: false };
    handle_plan_restructure(sink, db, payload, std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).await;
    let plan = loop {
        match events.recv().await.expect("plan event").payload {
            EventPayload::RestructurePlan(plan) => break plan.inner,
            EventPayload::Error(err) => panic!("planning failed: {}", err.inner.message),
            _ => {}
        }
    };
    let _ = std::fs::remove_dir_all(&root);

    assert!(!plan.moves.is_empty());
    let counts = plan.confidence_counts.expect("the plan carries confidence counts");
    let tally = |name: &str| plan.moves.iter().filter(|m| m.confidence.eq_ignore_ascii_case(name)).count() as u64;
    assert_eq!((counts.auto, counts.review, counts.ask), (tally("auto"), tally("review"), tally("ask")));
    assert_eq!(counts.auto + counts.review + counts.ask + counts.unknown, plan.moves.len() as u64);
    assert_eq!(plan.total_moves.unwrap_or(plan.moves.len() as u64), plan.moves.len() as u64);
}
