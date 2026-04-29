import Foundation

// Single source of truth for which file extensions FileID understands.
// Everything else (scan discovery, Library tab filter, category mapping,
// Cleanup badges) derives from these sets.

enum FileTypes {
    static let images: Set<String> = [
        "jpg","jpeg","png","heic","heif",
        "tif","tiff","gif","bmp","webp",
    ]

    static let videos: Set<String> = [
        "mp4","mov","m4v","avi","mkv","webm",
    ]

    static let pdfs: Set<String> = ["pdf"]

    static let officeWord: Set<String> = ["docx","doc"]
    static let officeSheet: Set<String> = ["xlsx","xls"]
    static let officeSlides: Set<String> = ["pptx","ppt"]

    static let openDocText: Set<String> = ["odt"]
    static let openDocSheet: Set<String> = ["ods"]
    static let openDocSlides: Set<String> = ["odp"]

    static let iWorkPages: Set<String> = ["pages"]
    static let iWorkNumbers: Set<String> = ["numbers"]
    static let iWorkKeynote: Set<String> = ["key"]

    static let richText: Set<String> = ["rtf","rtfd"]

    static let plainText: Set<String> = [
        "txt","md","markdown","log",
        "csv","tsv",
        "json","yaml","yml","toml",
        "xml","html","htm","plist",
        "srt","vtt",
    ]

    static let word: Set<String>        = officeWord.union(openDocText).union(iWorkPages)
    static let spreadsheet: Set<String> = officeSheet.union(openDocSheet).union(iWorkNumbers)
    static let presentation: Set<String> = officeSlides.union(openDocSlides).union(iWorkKeynote)

    static let documents: Set<String>   = pdfs
        .union(word).union(spreadsheet).union(presentation)
        .union(richText).union(plainText)

    static let all: Set<String> = images.union(videos).union(documents)
}
