enum FileKindLabel {
    /// Display name for a catalog `files.kind` value ("pdf" reads as "PDF", not "Pdf").
    static func title(_ kind: String) -> String {
        kind.lowercased() == "pdf" ? "PDF" : kind.capitalized
    }
}
