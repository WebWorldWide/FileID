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
        "raw", "cr2", "nef", "arw", "dng", "orf", "rw2", "raf"
    ]
    public static let videos: Set<String> = [
        "mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "flv", "mpg", "mpeg", "mts", "m2ts", "3gp"
    ]
    public static let pdfs: Set<String> = ["pdf"]
    public static let documents: Set<String> = [
        "pdf", "doc", "docx", "odt", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "md", "pages", "numbers", "key"
    ]
    public static let audio: Set<String> = [
        "mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "aiff"
    ]
    /// Source code + prose markup — read as UTF-8 text and clustered by content (BGE),
    /// classified as `.doc`. Lockstep with the Rust engine's FileKind::from_extension.
    public static let code: Set<String> = [
        "swift", "py", "rb", "js", "jsx", "ts", "tsx", "java", "kt", "c", "h", "cpp",
        "cc", "cxx", "hpp", "hh", "cs", "go", "rs", "php", "sh", "bash", "zsh", "sql",
        "scala", "m", "mm", "r", "jl", "lua", "dart", "vue", "pl", "pm", "ps1",
        "tex", "bib", "rst", "org", "adoc"
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
        if documents.contains(e) || code.contains(e) || ebooks.contains(e) { return .doc }
        if audio.contains(e)     { return .audio }
        if models.contains(e)    { return .model }
        return .other
    }

    public static func isTaggable(_ ext: String) -> Bool {
        let e = ext.lowercased()
        return images.contains(e) || videos.contains(e) || documents.contains(e)
            || code.contains(e) || ebooks.contains(e)
            || audio.contains(e) || models.contains(e)
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
