// What Chat shows before there is a library to talk to (macOS Chat
// "scan-first" state): an invitation to scan instead of an empty composer,
// then the composer once the first files are read.

const SUMMARY: &str = "Chat answers questions about the files FileID has scanned.";
const NOTHING_MOVED: &str = "Nothing is moved or changed.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Library<'a> {
    /// Files in the catalog so far.
    pub files: u64,
    pub scan_running: bool,
    /// Display name of the chosen folder, when one is chosen.
    pub folder: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invitation {
    ChooseFolder,
    ScanFolder(String),
}

impl Invitation {
    pub fn button(&self) -> String {
        match self {
            Self::ChooseFolder => "Choose a folder to scan".to_owned(),
            Self::ScanFolder(folder) => format!("Scan {folder}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intro {
    pub title: &'static str,
    pub body: String,
    pub invitation: Option<Invitation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanFirst {
    pub intro: Option<Intro>,
    pub composer_enabled: bool,
    pub note: Option<&'static str>,
}

pub const NO_LIBRARY_PLACEHOLDER: &str = "Scan a folder first";

pub fn state(library: Library<'_>) -> ScanFirst {
    if library.files > 0 {
        return ScanFirst {
            intro: None,
            composer_enabled: true,
            note: library.scan_running.then_some(
                "FileID is still reading your files. Answers cover the files read so far.",
            ),
        };
    }
    let (body, invitation) = match (library.folder, library.scan_running) {
        (Some(folder), true) => (
            format!("FileID is reading {folder} now. You can ask as soon as the first files are read. {NOTHING_MOVED}"),
            None,
        ),
        (Some(folder), false) => (
            format!(
                "{SUMMARY} FileID hasn't read {folder} yet. Scan it and FileID reads it on this computer. {NOTHING_MOVED}"
            ),
            Some(Invitation::ScanFolder(folder.to_owned())),
        ),
        (None, _) => (
            format!("{SUMMARY} Choose a folder and FileID reads it on this computer. {NOTHING_MOVED}"),
            Some(Invitation::ChooseFolder),
        ),
    };
    ScanFirst {
        intro: Some(Intro {
            title: "Scan a folder to start chatting",
            body,
            invitation,
        }),
        composer_enabled: false,
        note: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(files: u64, scan_running: bool, folder: Option<&str>) -> Library<'_> {
        Library {
            files,
            scan_running,
            folder,
        }
    }

    #[test]
    fn no_library_and_no_folder_asks_to_choose_one() {
        let state = state(library(0, false, None));
        let intro = state.intro.unwrap();
        assert_eq!(intro.title, "Scan a folder to start chatting");
        assert_eq!(
            intro.body,
            "Chat answers questions about the files FileID has scanned. Choose a folder and FileID reads it on this computer. Nothing is moved or changed."
        );
        assert_eq!(
            intro.invitation.as_ref().map(Invitation::button).as_deref(),
            Some("Choose a folder to scan")
        );
        assert!(!state.composer_enabled);
        assert_eq!(state.note, None);
    }

    #[test]
    fn a_chosen_but_unscanned_folder_is_named_on_the_button() {
        let state = state(library(0, false, Some("Eckerts 10-4-26")));
        let intro = state.intro.unwrap();
        assert!(intro
            .body
            .contains("FileID hasn't read Eckerts 10-4-26 yet. Scan it"));
        assert_eq!(
            intro.invitation,
            Some(Invitation::ScanFolder("Eckerts 10-4-26".to_owned()))
        );
        assert_eq!(intro.invitation.unwrap().button(), "Scan Eckerts 10-4-26");
        assert!(!state.composer_enabled);
    }

    #[test]
    fn the_first_scan_with_no_files_read_yet_offers_no_button_and_keeps_the_composer_off() {
        let state = state(library(0, true, Some("Christine")));
        let intro = state.intro.unwrap();
        assert_eq!(intro.invitation, None);
        assert!(intro.body.contains("FileID is reading Christine now"));
        assert!(!state.composer_enabled);
    }

    #[test]
    fn the_composer_opens_once_files_are_read_and_the_scan_is_still_going() {
        let state = state(library(934, true, Some("Christine")));
        assert_eq!(state.intro, None);
        assert!(state.composer_enabled);
        assert_eq!(
            state.note,
            Some("FileID is still reading your files. Answers cover the files read so far.")
        );
    }

    #[test]
    fn a_scanned_library_is_plain_chat() {
        let state = state(library(14_661, false, Some("Christine")));
        assert_eq!(
            state,
            ScanFirst {
                intro: None,
                composer_enabled: true,
                note: None
            }
        );
    }
}
