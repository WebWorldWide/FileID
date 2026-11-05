import Foundation

public enum FileKind: String, Sendable {
    // `model` = 3D models (scanned, not dropped like `other`) so Deep Analyze can
    // name them from their embedded object/material labels. Lockstep with the Rust
    // engine's FileKind::Model ("model"). Wavefront `.obj` only for now.
    case image, video, pdf, doc, audio, model, other
}

public enum FileTypes {
    // Conservative starting set; expand in M3 as we test more formats.
    public static let images: Set<String> = [
        "jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "webp", "gif", "bmp",
        "raw", "cr2", "nef", "arw", "dng", "orf", "rw2", "raf",
        "avif", "cr3", "srw", "pef", "3fr", "erf", "kdc", "mef", "mos", "mrw", "rwl", "srf", "x3f"
    ]
    public static let videos: Set<String> = [
        "mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "flv", "mpg", "mpeg", "mts", "m2ts", "3gp",
        "3g2", "mxf", "ogv", "vob", "f4v", "asf", "divx"
    ]
    public static let pdfs: Set<String> = ["pdf"]
    public static let documents: Set<String> = [
        "pdf", "doc", "docx", "odt", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "md", "pages", "numbers", "key"
    ]
    public static let audio: Set<String> = [
        "mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "aiff",
        "mp2", "mp1", "oga", "m4b", "aif", "aifc", "caf", "mka", "spx", "wma", "amr", "ape", "wv"
    ]
    /// Source code + prose markup — read as UTF-8 text and clustered by content (BGE),
    /// classified as `.doc`. Lockstep with the Rust engine's FileKind::from_extension.
    public static let code: Set<String> = [
        "swift", "py", "rb", "js", "jsx", "ts", "tsx", "java", "kt", "c", "h", "cpp",
        "cc", "cxx", "hpp", "hh", "cs", "go", "rs", "php", "sh", "bash", "zsh", "sql",
        "scala", "m", "mm", "r", "jl", "lua", "dart", "vue", "pl", "pm", "ps1",
        "tex", "bib", "rst", "org", "adoc"
    ]
    /// Plain-text data and logs — read as UTF-8 and clustered by content as `.doc`.
    /// Lockstep with the Rust engine's FileKind::from_extension.
    public static let textData: Set<String> = [
        "csv", "tsv", "log", "markdown", "text", "json", "yaml", "yml", "toml", "xml", "html", "htm"
    ]
    /// E-books — extracted to text and clustered as `.doc`. EPUB only (zip of XHTML);
    /// MOBI is proprietary (low ROI). Lockstep with the Rust engine.
    public static let ebooks: Set<String> = ["epub"]
    /// 3D models — rendered to a thumbnail and (for `.obj`) clustered by CLIP like images;
    /// every recognized format is grouped under `3D Models/` + named by Deep Analyze.
    /// Lockstep with the Rust engine's FileKind::from_extension.
    public static let models: Set<String> = [
        "obj", "stl", "ply", "glb", "gltf", "fbx", "usdz", "usd", "usda", "usdc",
        "dae", "3mf", "3ds", "off"
    ]

    public static func kind(forExtension ext: String) -> FileKind {
        let e = ext.lowercased()
        if images.contains(e)    { return .image }
        if videos.contains(e)    { return .video }
        if pdfs.contains(e)      { return .pdf }
        if documents.contains(e) || code.contains(e) || textData.contains(e) || ebooks.contains(e) { return .doc }
        if audio.contains(e)     { return .audio }
        if models.contains(e)    { return .model }
        return .other
    }

    public static func isTaggable(_ ext: String) -> Bool {
        let e = ext.lowercased()
        return images.contains(e) || videos.contains(e) || documents.contains(e)
            || code.contains(e) || textData.contains(e) || ebooks.contains(e)
            || audio.contains(e) || models.contains(e)
    }

    /// Other files are cataloged with name, size, dates and a content hash so they
    /// count, search and appear in Duplicates. Never these: half-finished downloads,
    /// editor scratch files and per-folder system files (mirrors the shared engine).
    public static let neverCataloged: Set<String> = [
        "part", "partial", "crdownload", "download", "opdownload", "tmp", "temp", "swp", "swo", "lock",
        "sqlite-wal", "sqlite-shm", "sqlite-journal", "db-wal", "db-shm", "db-journal"
    ]
    public static let neverCatalogedNames: Set<String> = ["desktop.ini", "thumbs.db", "icon\r", "ehthumbs.db"]

    /// True for a file the scan catalogs: a known kind, or any other file that is not junk.
    public static func isCataloged(name: String) -> Bool {
        let lower = name.lowercased()
        guard !neverCatalogedNames.contains(lower) else { return false }
        let ext = (lower as NSString).pathExtension
        return isTaggable(ext) || !neverCataloged.contains(ext)
    }

    public static let archives: Set<String> = ["zip", "tar", "gz", "rar", "7z"]
    public static let structuredData: Set<String> = ["json", "yaml", "yml", "toml", "xml"]

    /// SF Symbol for a file: code, archives and data files get their own glyph;
    /// everything else uses its kind's.
    public static func symbolName(forExtension ext: String) -> String {
        let e = ext.lowercased()
        if code.contains(e) { return "chevron.left.forwardslash.chevron.right" }
        if archives.contains(e) { return "doc.zipper" }
        if structuredData.contains(e) { return "curlybraces.square" }
        return kind(forExtension: e).symbolName
    }

    public static func isVideo(path: String) -> Bool {
        videos.contains((path as NSString).pathExtension.lowercased())
    }
}

extension FileKind {
    public var symbolName: String {
        switch self {
        case .image: "photo"
        case .video: "video"
        case .pdf: "doc.richtext"
        case .doc: "doc.text"
        case .audio: "waveform"
        case .model: "cube"
        case .other: "doc"
        }
    }

    public static func symbolName(forKind raw: String) -> String {
        FileKind(rawValue: raw)?.symbolName ?? "doc"
    }
}
