use super::chat::execute;
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

