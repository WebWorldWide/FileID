extension DeepAnalyze.AnalysisResult {
    /// A result built by a host (a failure, metadata naming, an empty
    /// placeholder); the memberwise initializer is internal to this module.
    package static func make(description: String, proposedName: String?, tags: [String] = []) -> Self {
        var result = Self(description: description, proposedName: proposedName)
        result.tags = tags
        return result
    }
}
