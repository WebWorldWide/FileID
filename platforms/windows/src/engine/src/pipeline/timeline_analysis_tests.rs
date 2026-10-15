use super::*;

fn close(lhs: f64, rhs: f64) -> bool {
    (lhs - rhs).abs() < 1e-9
}

fn signal(seconds: f64, change_score: f64) -> Signal {
    Signal { seconds, change_score }
}

#[test]
fn change_score_is_normalized_mean_absolute_difference() {
    assert!(close(change_score(&[0, 0], &[255, 255]), 1.0));
    assert!(close(change_score(&[10, 20], &[10, 20]), 0.0));
    assert!((change_score(&[0, 0], &[51, 0]) - 0.1).abs() < 1e-9);
    assert!(close(change_score(&[1], &[1, 2]), 0.0));
}

#[test]
fn signals_compare_across_chunk_boundary() {
    let frames = vec![(60.0, vec![255; 4]), (61.0, vec![255; 4])];
    let first_chunk_only = signals_from_frames(&frames, None);
    assert_eq!(first_chunk_only, vec![signal(61.0, 0.0)]);
    let with_previous = signals_from_frames(&frames, Some(&[0; 4]));
    assert_eq!(with_previous, vec![signal(60.0, 1.0), signal(61.0, 0.0)]);
}

#[test]
fn sample_times_prefer_peaks_and_fall_back_to_boundaries() {
    assert_eq!(sample_times(30.0, 10.0, &[]), vec![0.0, 10.0, 20.0]);
    let peaks = [signal(4.0, 0.3), signal(6.0, 0.1), signal(15.0, 0.01)];
    assert_eq!(sample_times(30.0, 10.0, &peaks), vec![0.0, 4.0, 20.0]);
}

#[test]
fn sample_times_respect_spacing_and_reject_invalid_input() {
    assert_eq!(sample_times(12.0, 10.0, &[signal(0.5, 0.9)]), vec![0.0, 10.0]);
    assert!(sample_times(f64::NAN, 10.0, &[]).is_empty());
    assert!(sample_times(10.0, 0.0, &[]).is_empty());
    let ties = [signal(7.0, 0.5), signal(3.0, 0.5)];
    assert_eq!(sample_times(10.0, 10.0, &ties), vec![0.0, 3.0]);
}

#[test]
fn moment_windows_overlap_and_place_peak() {
    let result = windows(10.0, &[signal(5.2, 0.5)]);
    assert_eq!(result.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>(), vec![(0.0, 4.0), (3.0, 7.0), (6.0, 10.0)]);
    assert!(result.iter().all(|w| w.times.len() == 8));
    assert!(result[1].times.contains(&5.2));
    assert!(result[1].times.windows(2).all(|p| p[0] <= p[1]));
    assert!(close(result[0].times[0], 0.0));
    assert!(windows(0.0, &[]).is_empty());
    assert!(windows(MAX_DURATION_SECONDS + 1.0, &[]).is_empty());
}

#[test]
fn short_video_gets_single_window() {
    let result = windows(2.0, &[]);
    assert_eq!(result.len(), 1);
    assert_eq!((result[0].start, result[0].end), (0.0, 2.0));
}

#[test]
fn parse_moments_accepts_wrapped_json_and_rejects_bad_ranges() {
    let raw = "Sure:\n{\"moments\":[{\"title\":\"Kid swings bat\",\"summary\":\"A child swings.\",\"firstFrame\":1,\"lastFrame\":3}]}";
    let moments = parse_moments(raw, 4).unwrap();
    assert_eq!(moments[0].title, "Kid swings bat");
    assert!(parse_moments("{\"moments\":[]}", 4).unwrap().is_empty());
    assert!(parse_moments(raw, 3).is_err());
    assert!(parse_moments(raw, 1).is_err());
    assert!(parse_moments("no json", 4).is_err());
    let blank = "{\"moments\":[{\"title\":\"  \",\"summary\":\"\",\"firstFrame\":0,\"lastFrame\":0}]}";
    assert!(parse_moments(blank, 4).is_err());
    let reversed = "{\"moments\":[{\"title\":\"x\",\"summary\":\"\",\"firstFrame\":2,\"lastFrame\":1}]}";
    assert!(parse_moments(reversed, 4).is_err());
}

#[test]
fn distinct_frames_drop_repeated_timestamps() {
    let window = Window { start: 0.0, end: 4.0, times: vec![] };
    assert_eq!(distinct_frame_indices(&[0.0, 0.0, 1.0, 1.0, 2.0], &window).unwrap(), vec![0, 2, 4]);
    assert!(distinct_frame_indices(&[1.0, 1.0], &window).is_err());
    assert!(distinct_frame_indices(&[2.0, 1.0], &window).is_err());
    assert!(distinct_frame_indices(&[0.0, 5.0], &window).is_err());
}

#[test]
fn moment_chapters_map_frames_to_intervals() {
    let window = Window { start: 3.0, end: 7.0, times: vec![] };
    let moments = vec![
        Moment { title: " Catch ".into(), summary: "Ball caught.".into(), first_frame: 1, last_frame: 2 },
        Moment { title: "End".into(), summary: String::new(), first_frame: 3, last_frame: 3 },
        Moment { title: "Out of range".into(), summary: String::new(), first_frame: 2, last_frame: 9 },
    ];
    let chapters = moment_chapters(&moments, &[3.0, 4.0, 5.0, 6.0], &window, 7, "10:abc", "v");
    assert_eq!(chapters.len(), 2);
    assert_eq!((chapters[0].start_seconds, chapters[0].end_seconds, chapters[0].title.as_str()), (4.0, 6.0, "Catch"));
    assert_eq!((chapters[1].start_seconds, chapters[1].end_seconds), (6.0, 7.0));
    assert_eq!(chapters[0].id, "timeline-moment:7:10:abc:3000:0");
    assert!(!chapters[0].user_edited && close(chapters[0].confidence, 0.2));
}

#[test]
fn chapter_suggestions_split_at_confirmed_transitions() {
    let captions = [
        Caption { seconds: 0.0, text: "A birthday cake with candles on a table".into() },
        Caption { seconds: 10.0, text: "Birthday cake candles burning on table".into() },
        Caption { seconds: 20.0, text: "Children playing soccer in a park".into() },
        Caption { seconds: 30.0, text: "Children kicking soccer ball in park".into() },
    ];
    let chapters = chapter_suggestions(1, "rev", "frame-v", 40.0, &captions);
    assert_eq!(chapters.len(), 2);
    assert_eq!((chapters[0].start_seconds, chapters[1].start_seconds), (0.0, 20.0));
    assert!((chapters[0].end_seconds - 19.999).abs() < 1e-9);
    assert!(close(chapters[1].end_seconds, 40.0));
    assert_eq!(chapters[1].model_version, "timeline-chapter-suggestion-v1/frame-v");
    assert!(chapter_suggestions(1, "rev", "v", 40.0, &captions[..2]).is_empty());
}

#[test]
fn concise_titles_cut_at_sentence_and_word_boundaries() {
    assert_eq!(concise_title("A dog runs. Then it sits."), "A dog runs");
    let long = "word ".repeat(30);
    let title = concise_title(&long);
    assert!(title.ends_with('…') && title.chars().count() <= 89);
    assert_eq!(concise_title(".Leading dot"), "Leading dot");
}

#[test]
fn prompt_lists_every_frame_time() {
    let prompt = moment_prompt(&[0.0, 1.5]);
    assert!(prompt.contains("frame 0: 0.000 seconds, frame 1: 1.500 seconds"));
    assert!(prompt.contains("untrusted content"));
}

#[test]
fn speech_chunks_cover_the_file_without_overlap() {
    assert_eq!(speech_chunks(65.0, 30.0), vec![(0.0, 30.0), (30.0, 60.0), (60.0, 65.0)]);
    assert_eq!(speech_chunks(60.4, 30.0), vec![(0.0, 30.0), (30.0, 60.4)], "a sub-second tail joins the previous chunk");
    assert_eq!(speech_chunks(0.5, 30.0), vec![(0.0, 0.5)]);
    assert!(speech_chunks(0.0, 30.0).is_empty());
    assert!(speech_chunks(f64::NAN, 30.0).is_empty());
}

#[test]
fn transcripts_strip_model_framing() {
    assert_eq!(transcript_text("language English<asr_text>Happy birthday, Sam!"), "Happy birthday, Sam!");
    assert_eq!(transcript_text("  Plain transcript.  "), "Plain transcript.");
    assert_eq!(transcript_text("language None<asr_text>"), "");
    assert_eq!(transcript_text("[SILENCE]"), "");
    assert_eq!(transcript_text("<asr_text>Hi</asr_text>"), "Hi");
}
