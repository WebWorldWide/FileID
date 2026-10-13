use super::*;

#[test]
fn answers_normalize_to_comparable_tokens() {
    assert_eq!(normalize_answer("3"), "3");
    assert_eq!(normalize_answer("Three."), "3");
    assert_eq!(normalize_answer("There are 5 circles"), "5");
    assert_eq!(normalize_answer("Blue."), "blue");
    assert_eq!(normalize_answer("The answer is green"), "green");
    assert_eq!(normalize_answer("12345"), "12345");
    assert_eq!(normalize_answer("  "), "");
}

#[test]
fn median_handles_even_odd_and_empty() {
    assert_eq!(median(&[]), None);
    assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
    assert_eq!(median(&[f64::NAN, 1.0]), Some(1.0));
}

#[test]
fn probes_render_deterministically_with_expected_answers() {
    let directory = std::env::temp_dir().join(format!("fileid-bench-probes-{}", uuid::Uuid::new_v4()));
    let probes = write_probes(&directory).unwrap();
    assert_eq!(probes.iter().map(|p| p.expected).collect::<Vec<_>>(), ["3", "blue", "5", "green", "12345"]);
    for probe in &probes {
        let image = image::open(&probe.image).unwrap();
        assert!(image.width() >= 400 && image.height() >= 100, "{}", probe.name);
    }
    let first = std::fs::read(&probes[0].image).unwrap();
    let again = write_probes(&directory).unwrap();
    assert_eq!(first, std::fs::read(&again[0].image).unwrap());
    let square = image::open(&probes[1].image).unwrap().to_rgb8();
    assert_eq!(square.get_pixel(224, 224).0, BLUE);
    assert_eq!(square.get_pixel(10, 10).0, WHITE);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn summary_reports_rates_and_omits_unmeasured_fields() {
    let measurement = Measurement {
        load_seconds: 3.456,
        prompt_rates: vec![800.0, 1000.0],
        generated_rates: vec![40.0, 50.0, 60.0],
        caption_seconds: vec![1.0, 2.0, 3.0],
        correct: 4,
        probes: 5,
        json_valid: 1,
        json_total: 2,
        peak_resident_bytes: 0,
    };
    let summary = summarize(&measurement, "test-hardware", Some(vec!["-ngl".into(), "0".into()]));
    assert_eq!(summary.load_seconds, 3.46);
    assert_eq!(summary.prompt_tokens_per_second, Some(900.0));
    assert_eq!(summary.generated_tokens_per_second, Some(50.0));
    assert_eq!(summary.seconds_per_image, Some(2.0));
    assert_eq!(summary.probe_accuracy, Some(0.8));
    assert_eq!(summary.json_valid_rate, Some(0.5));
    assert_eq!(summary.peak_resident_gb, None);
    let empty = summarize(&Measurement::default(), "h", None);
    assert!(empty.probe_accuracy.is_none() && empty.json_valid_rate.is_none() && empty.generated_tokens_per_second.is_none());
}

#[tokio::test]
async fn tuning_keeps_fastest_identical_output_only() {
    let baseline = ("a caption".to_owned(), 20.0);
    let flags = |n: &str| vec!["-t".to_owned(), n.to_owned()];
    let outcomes = std::collections::HashMap::from([
        ("2", Ok(("a caption".to_owned(), 25.0))),
        ("4", Ok(("a different caption".to_owned(), 90.0))),
        ("8", Ok(("a caption".to_owned(), 30.0))),
        ("16", Err(anyhow::anyhow!("did not start"))),
    ]);
    let outcomes = std::sync::Mutex::new(outcomes);
    let launch = |args: Vec<String>| {
        let result = outcomes.lock().unwrap().remove(args[1].as_str()).unwrap();
        async move { result }
    };
    let winner = tune(&baseline, vec![flags("2"), flags("4"), flags("8"), flags("16")], launch).await;
    assert_eq!(winner, Some((flags("8"), 30.0)), "faster but different output is rejected");
    let slow = tune(&baseline, vec![flags("2")], |_| async { Ok(("a caption".to_owned(), 20.5)) }).await;
    assert_eq!(slow, None, "a gain under 5% keeps the defaults");
}

#[test]
fn stored_results_round_trip_per_model_and_hardware() {
    let directory = std::env::temp_dir().join(format!("fileid-bench-store-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("model-benchmarks.json");
    let benchmark = summarize(&Measurement { load_seconds: 1.0, ..Measurement::default() }, "h1", None);
    save_in(&path, StoredBenchmark { model: "m".into(), hardware: "h1".into(), benchmark: benchmark.clone() }).unwrap();
    save_in(&path, StoredBenchmark { model: "m".into(), hardware: "h2".into(), benchmark: benchmark.clone() }).unwrap();
    let newer = ModelBenchmark { load_seconds: 2.0, ..benchmark };
    save_in(&path, StoredBenchmark { model: "m".into(), hardware: "h1".into(), benchmark: newer.clone() }).unwrap();
    let stored = load_in(&path);
    assert_eq!(stored.len(), 2);
    assert_eq!(stored.iter().find(|r| r.hardware == "h1").unwrap().benchmark, newer);
    std::fs::write(&path, b"not json").unwrap();
    assert!(load_in(&path).is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}
