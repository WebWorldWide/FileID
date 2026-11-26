use super::chat::execute;
use super::chat_actions::model_proposals;
use crate::ipc::{ChatRequest, ChatResponse};
use rusqlite::Connection;

fn ask_in(conn: &mut Connection, conversation: &str, text: &str) -> ChatResponse {
    let request = ChatRequest {
        request_id: format!("{conversation}-{text}"),
        conversation_id: conversation.into(),
        action: "send".into(),
        text: Some(text.into()),
        use_model: Some(false),
        ..Default::default()
    };
    execute(conn, &request).unwrap()
}

fn ask(conn: &mut Connection, text: &str) -> ChatResponse {
    ask_in(conn, text, text)
}

fn library() -> Connection {
    let conn = crate::db::test_support::memory_db();
    conn.execute_batch(
        "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,scanned_at,kind,extension,vlm_description,location_lat,location_lon) VALUES
         (1,'/lib/Ryan Homecoming Photos 9-26-26/a.jpg',1,100,1688472000,0,'image','jpg','A dog',37.5,-122.25),
         (2,'/lib/Ryan Homecoming Photos 9-26-26/b.mp4',2,100,1688472001,0,'video','mp4',NULL,NULL,NULL),
         (3,'/lib/Other/DSC02096.jpg',3,100,1688472002,0,'image','jpg',NULL,NULL,NULL),
         (4,'/lib/Other/DSC02097.jpg',4,100,1688472003,0,'image','jpg',NULL,40.25,-74.5),
         (5,'/lib/Google Takeout Export/Photos/h.jpg',5,10,1688472009,0,'image','jpg',NULL,NULL,NULL),
         (6,'/lib/Google Takeout Export/Photos/i.jpg',6,10,1688472010,0,'image','jpg',NULL,NULL,NULL),
         (7,'/lib/Mail/Takeout notes/MyActivity.html',7,10,1688472011,0,'other','html','Google activity',NULL,NULL);
         INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed,error_message) VALUES
         (20,'/lib/Mail/broken.pdf',20,10,0,'pdf','pdf',1,'Could not parse the file'),
         (21,'/lib/Mail/broken2.pdf',21,10,0,'pdf','pdf',1,'Could not parse the file'),
         (22,'/lib/Mail/broken3.jpg',22,10,0,'image','jpg',1,'Image decode failed');",
    )
    .unwrap();
    conn
}

#[test]
fn whats_in_a_folder_reads_the_folders_that_begin_with_its_name_and_the_next_question_stays_there() {
    let mut conn = library();
    let inside = ask_in(&mut conn, "ryan", "What's in the Ryan Homecoming folder?");
    assert!(inside.message.starts_with("“Ryan Homecoming” holds 2 files"), "{}", inside.message);
    assert_eq!(inside.hits.len(), 2);
    let photos = ask_in(&mut conn, "ryan", "How many photos are in it?");
    assert!(photos.message.starts_with("Found 1 photo in Ryan Homecoming"), "{}", photos.message);
}

#[test]
fn a_duration_question_says_chat_cannot_filter_by_length() {
    let mut conn = library();
    for text in ["How many videos longer than 10 minutes?", "Show videos shorter than 30 seconds", "Which clips are over an hour long?"] {
        let answer = ask(&mut conn, text);
        assert!(answer.message.starts_with("FileID doesn't know how long videos run"), "{text}: {}", answer.message);
        assert!(answer.hits.is_empty(), "{text}");
    }
    assert!(ask(&mut conn, "How many videos do I have?").message.starts_with("Found 1 video"));
}

#[test]
fn folder_file_type_and_description_counts_come_from_the_catalog() {
    let mut conn = library();
    for text in ["How many folders do I have?", "How many folders are in my library?"] {
        assert!(ask(&mut conn, text).message.starts_with("Your library has 4 folders holding 7 files."), "{text}");
    }
    let formats = ask(&mut conn, "How many formats do I have?").message;
    assert!(formats.starts_with("Your 7 files use 3 formats: 5 JPG"), "{formats}");
    let kinds = ask(&mut conn, "How many file types do I have?").message;
    assert!(kinds.starts_with("Your library has 7 files using"), "{kinds}");
    assert_eq!(ask(&mut conn, "How many files have descriptions?").message, "1 of 6 files have a description. 5 still need one; ask me to describe them.");
}

#[test]
fn questions_the_catalog_cannot_answer_say_what_chat_can_do_instead_of_searching_their_words() {
    let mut conn = library();
    for text in ["How many files have the same name?", "What is the average file size?", "Why is my library so big?"] {
        let answer = ask(&mut conn, text);
        assert!(answer.message.starts_with("I can't answer that from the catalog."), "{text}: {}", answer.message);
        assert!(answer.hits.is_empty(), "{text}");
    }
}

#[test]
fn where_was_a_named_file_taken_reads_that_files_position() {
    let mut conn = library();
    let none = ask(&mut conn, "Where was the photo DSC02096 taken?");
    assert_eq!(none.message, "DSC02096.jpg has no location recorded, so I can't say where it was taken.");
    assert_eq!(none.hits.len(), 1);
    let some = ask(&mut conn, "Where was DSC02097 taken?");
    assert_eq!(some.message, "DSC02097.jpg has a location recorded (40.2500, -74.5000), but I can't name places yet.");
    assert_eq!(ask(&mut conn, "Where was DSC09999 taken?").message, "I can't find a file called DSC09999 in your library.");
    assert!(ask(&mut conn, "Where were my photos taken?").message.contains("of your"), "a question naming no file still reads the library");
}

#[test]
fn why_files_could_not_be_read_lists_the_recorded_reasons() {
    let mut conn = library();
    let answer = ask(&mut conn, "Why couldn't they be read?");
    assert_eq!(answer.message, "3 files couldn't be read. Reasons: “Could not parse the file” (2 files), “Image decode failed” (1 file).");
    assert_eq!(answer.hits.len(), 3);
}

#[test]
fn a_model_answer_loses_its_closing_no_action_sentence() {
    let (prose, intents) = model_proposals("Five photos show a watch [1][2]. No action is requested, so no action is proposed.");
    assert_eq!(prose, "Five photos show a watch [1][2].");
    assert!(intents.is_empty());
    let (prose, _) = model_proposals("Two photos.\nNo action is requested.\n<actions>[]</actions>");
    assert_eq!(prose, "Two photos.");
    let (prose, _) = model_proposals("No action shots were found in this folder.");
    assert_eq!(prose, "No action shots were found in this folder.");
}

#[test]
fn a_named_folder_of_files_counts_the_same_files_however_it_is_asked() {
    let mut conn = library();
    let by_folder = ask(&mut conn, "How many files are in the Google Takeout folder?");
    let by_name = ask(&mut conn, "How many Google Takeout files do I have?");
    assert!(by_folder.message.starts_with("Found 2 files in Google Takeout"), "{}", by_folder.message);
    assert!(by_name.message.starts_with("Found 2 files in Google Takeout"), "{}", by_name.message);
}

#[test]
fn the_rename_card_and_library_smart_names_use_one_style() {
    let mut conn = crate::db::test_support::memory_db();
    let dir = std::env::temp_dir().join(format!("fileid-chat-names-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    crate::db::test_support::insert_file(&conn, 1, &dir.join("DSC02096.jpg").to_string_lossy(), "image");
    conn.execute("UPDATE files SET vlm_proposed_name='Person Pointing At Car Tire' WHERE id=1", []).unwrap();
    let request: crate::ipc::CatalogRequest = serde_json::from_value(serde_json::json!({
        "requestID": "names", "action": "proposeNames", "fileIDs": [1],
        "naming": {"style": "short", "maxLength": 40, "includePeople": true, "includeEvent": true, "learn": true}
    }))
    .unwrap();
    let library_name = super::catalog_names::propose(&conn, &request).unwrap().remove(0).text;
    assert!(!library_name.is_empty());
    let card = ask(&mut conn, "Rename my photos to better names");
    let renames = card.actions.unwrap_or_else(|| panic!("{}", card.message)).remove(0).renames.expect("renames");
    assert_eq!(renames.iter().map(|rename| rename.new_name.as_str()).collect::<Vec<_>>(), vec![library_name.as_str()]);
    assert!(library_name.chars().all(|c| c.is_lowercase() || c == '-' || c == '.' || c == '_' || c.is_ascii_digit()), "{library_name}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_folder_question_with_a_condition_is_not_answered_with_the_plain_folder_count() {
    let mut conn = library();
    assert!(ask(&mut conn, "How many folders do I have?").message.starts_with("Your library has"));
    for question in ["How many folders have photos?", "How many folders are empty?", "How many folders have more than 100 files?"] {
        assert!(ask(&mut conn, question).message.starts_with("I can't answer that"), "{question}");
    }
}

#[test]
fn a_location_question_may_name_the_file_with_its_extension() {
    let mut conn = library();
    let answer = ask(&mut conn, "Where was DSC02097.jpg taken?").message;
    assert!(answer.contains("has a location recorded"), "{answer}");
    assert!(ask(&mut conn, "Where was DSC02096.jpg taken?").message.contains("no location recorded"));
}

#[test]
fn a_model_answer_that_is_only_the_no_action_sentence_is_kept() {
    let (prose, _) = model_proposals("No action is requested.");
    assert_eq!(prose, "No action is requested.");
}
