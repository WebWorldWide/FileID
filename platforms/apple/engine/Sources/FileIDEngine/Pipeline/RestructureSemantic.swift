// Butler restructure — semantic + learn-your-style classification.
//
// Swift mirror of the Windows engine's `restructure_semantic.rs` (see
// shared/docs/RESTRUCTURE.md). Where the legacy `Restructure.proposeAll`
// rule cascade buckets every photo into Photos/<Year>/<Month>, this fuses the
// rich signals already in the DB — CLIP image embedding + content tags +
// capture time — into one feature vector, clusters files by *content* (reusing
// the proven `IdentityClustering` density algorithm — no new deps), then
// assigns each cluster to the user's nearest EXISTING folder when the match is
// confident ("organize like I already do"), otherwise proposes a new
// distinctively-named group. Density-noise files fall back to the rule cascade.
//
// Pure logic + the engine plumbing live together here; the DB load lives in
// `Restructure.proposeAll`. Stays byte-faithful with the Rust implementation so
// a library round-trips across platforms.
import Foundation
import FileIDShared

public enum RestructureSemantic {

    /// Three-band autonomy tier for a single proposed move (RESTRUCTURE.md §6).
    /// Orthogonal to the folder Anchor/Mixed/Junk classification.
    public enum Confidence: String, Sendable {
        case auto, review, ask
    }

    /// Per-file signals. `clip` is the L2-normalized 512-d CLIP image embedding;
    /// callers only pass files that have one (images), so it is never empty.
    public struct SemanticFile: Sendable {
        public let fileID: Int64
        public let source: String
        public let clip: [Float]
        public let tags: [String]
        public let timeUnix: Double
        public init(fileID: Int64, source: String, clip: [Float], tags: [String], timeUnix: Double) {
            self.fileID = fileID
            self.source = source
            self.clip = clip
            self.tags = tags
            self.timeUnix = timeUnix
        }
    }

    /// A discovered placement for one file: where it goes, why, and how sure.
    public struct Move: Sendable {
        public let fileID: Int64
        public let source: String
        public let destinationDir: String
        public let category: String
        public let confidence: Confidence
        public let reason: String
    }

    /// An existing folder learned from the current tree: its path + the mean
    /// (L2-normalized) CLIP embedding of the files currently in it.
    public struct FolderPrototype: Sendable {
        public let path: String
        public let centroid: [Float]
    }

    /// A fusion-weight + confidence-threshold profile. The image pass keeps the
    /// calibrated CLIP-embedding values; the non-image pass (filename+tag
    /// bag-of-words representative — there is no CLIP image vector for a PDF or a
    /// video) runs a separate, deliberately tighter profile so a sparser signature
    /// can't over-move. Both stay byte-faithful with the Rust engine. (RESTRUCTURE.md §2/R1)
    public struct Profile: Sendable {
        public let wClip: Float        // weight on the representative content vector
        public let wTags: Float        // weight on the content-tag multi-hot block
        public let wTime: Float        // weight on the cyclical-time block
        public let folderMatchCos: Float
        public let autoFolderCos: Float
        public let autoCohesion: Float
        public let reviewCohesion: Float
        public let minMargin: Float
        public let autoMinMembers: Int
    }

    // Image pass — representative is the L2-normalized 512-d CLIP image embedding.
    // Values are unchanged from the original calibrated constants.
    public static let imageProfile = Profile(
        wClip: 0.70, wTags: 0.22, wTime: 0.08,
        folderMatchCos: 0.55, autoFolderCos: 0.72, autoCohesion: 0.62,
        reviewCohesion: 0.50, minMargin: 0.05, autoMinMembers: 4)

    private static let tagVocabCap = 256
    // Filenames tokenize into many one-off terms, so the non-image bag-of-words
    // needs a wider vocab than the image tag block.
    static let nonImageVocabCap = 512

    /// Owner kill-switch for the non-image semantic pass (`FILEID_RESTRUCTURE_NONIMAGE=0`
    /// → off, falls back to the rule cascade). Default on. (RESTRUCTURE.md R1)
    static var nonImageEnabled: Bool {
        ProcessInfo.processInfo.environment["FILEID_RESTRUCTURE_NONIMAGE"] != "0"
    }

    /// Non-image pass — representative is a filename+tag bag-of-words (sparser than
    /// CLIP), so demand a cleaner cluster + a tighter folder match. `wTags` is 0:
    /// tags already live inside the representative, and naming reads them directly,
    /// so re-adding the block would double-count. Thresholds are env-overridable
    /// (`FILEID_RESTRUCTURE_NI_*`) for owner calibration on a real library before
    /// the defaults are promoted. (RESTRUCTURE.md R1)
    static var nonImageProfile: Profile {
        Profile(
            wClip: 0.74, wTags: 0.0, wTime: 0.08,
            folderMatchCos: envFloat("FILEID_RESTRUCTURE_NI_FOLDER_COS", 0.60),
            autoFolderCos: envFloat("FILEID_RESTRUCTURE_NI_AUTO_FOLDER_COS", 0.80),
            autoCohesion: envFloat("FILEID_RESTRUCTURE_NI_AUTO_COH", 0.70),
            reviewCohesion: envFloat("FILEID_RESTRUCTURE_NI_REVIEW_COH", 0.55),
            minMargin: 0.08, autoMinMembers: 4)
    }

    private static func envFloat(_ key: String, _ dflt: Float) -> Float {
        guard let s = ProcessInfo.processInfo.environment[key], let v = Float(s) else { return dflt }
        return v
    }

    /// SOTA single-knob (HDBSCAN `min_cluster_size` philosophy, deep-research
    /// 2026-06-16): one `FILEID_RESTRUCTURE_GRANULARITY` ∈ {loose, normal, tight}
    /// shifts the cluster cosines so the owner tunes folder count with ONE lever.
    /// Loose = lower bar = broader / fewer folders; tight = higher = more. Applied
    /// identically on both engines so the chosen granularity round-trips.
    static func granularityDelta() -> Float {
        switch ProcessInfo.processInfo.environment["FILEID_RESTRUCTURE_GRANULARITY"] {
        case "loose": return -0.05
        case "tight": return 0.05
        default: return 0.0   // "normal" / unset
        }
    }

    private static func fileHyperparams() -> IdentityClustering.Hyperparameters {
        // Looser than faces: a semantic group is broader than one identity.
        let d = granularityDelta()
        return IdentityClustering.Hyperparameters(
            pass1Cosine: 0.50 + d, pass2Cosine: 0.40 + d, pass2Margin: 0.08,
            pass3VarianceThreshold: 0.06, pass3MinMeanCosine: 0.42 + d,
            pass3MaxSplits: 5, kNN: 12)
    }

    /// Build prototypes from the files' *current* locations: each parent folder
    /// with ≥ `minFiles` becomes a class whose centroid is the mean CLIP vector
    /// of its contents (Nearest-Class-Mean / Dropbox "Smart Move"). Zero user
    /// effort — the existing tree is the labeled ground truth.
    public static func folderPrototypes(_ files: [SemanticFile], minFiles: Int) -> [FolderPrototype] {
        var byFolder: [String: [[Float]]] = [:]
        for f in files {
            let parent = (f.source as NSString).deletingLastPathComponent
            byFolder[parent, default: []].append(f.clip)
        }
        var out: [FolderPrototype] = []
        for (path, vecs) in byFolder where vecs.count >= minFiles {
            if let centroid = meanUnit(vecs) {
                out.append(FolderPrototype(path: path, centroid: centroid))
            }
        }
        // Deterministic order (path) so proposals are stable across runs.
        return out.sorted { $0.path < $1.path }
    }

    /// Classify `files` into proposed moves: each discovered cluster either
    /// extends the nearest confident existing folder or becomes a new
    /// distinctively-named group under `libraryRoot`. Density-noise / singleton
    /// files are not returned — the caller routes the rest through its rule
    /// cascade fallback.
    public static func classify(
        files: [SemanticFile],
        prototypes: [FolderPrototype],
        libraryRoot: String,
        profile: Profile = imageProfile
    ) -> [Move] {
        guard !files.isEmpty else { return [] }
        let globalFreq = tagFrequencies(files)
        let vocab = vocabFromFreq(globalFreq, cap: tagVocabCap)
        let fused = files.map { fuse($0, vocab: vocab, profile: profile) }
        let clusterIDs = cluster(fused)

        var clusters: [Int: [Int]] = [:]
        for (i, cid) in clusterIDs.enumerated() { clusters[cid, default: []].append(i) }

        var moves: [Move] = []
        // Group names already claimed by a *different* new-group cluster this
        // run. Without this, two clusters with identical top tags collapse into
        // one folder (#9). Consulted ONLY by the new-group branch; the
        // existing-folder branch legitimately routes many clusters into one
        // user folder. Tracked in the SANITIZED namespace that actually backs
        // the directory. (F-C3-014)
        var usedGroupNames = Set<String>()
        // Stable cluster iteration (smallest id first) so the dedup below is
        // deterministic across runs.
        for cid in clusters.keys.sorted() {
            let members = clusters[cid]!
            // Singletons (the clusterer's outliers) have no group signal.
            guard members.count >= 2 else { continue }
            let memberClip = members.map { files[$0].clip }
            guard let centroid = meanUnit(memberClip) else { continue }
            let coh = cohesion(memberClip, centroid)

            let destDir: String
            let category: String
            let confidence: Confidence
            let reason: String

            // Containment guard: only an in-root prototype is a valid
            // destination — routing to a folder outside libraryRoot would be
            // rejected by the apply layer (canonicalizes outside root), so such
            // a match falls through to a new in-root group instead. (E12, F-C3-015)
            if let (proto, sim, runnerUp) = nearestTwoFolders(centroid, prototypes),
               sim >= profile.folderMatchCos, Self.pathContained(proto.path, in: libraryRoot) {
                // Learn-your-style: route to the nearest confident existing
                // folder. Auto only when strong AND unambiguous on a tight cluster.
                let name = (proto.path as NSString).lastPathComponent
                category = name.isEmpty ? "Folder" : name
                destDir = proto.path
                confidence = (sim >= profile.autoFolderCos && coh >= profile.reviewCohesion && (sim - runnerUp) >= profile.minMargin)
                    ? .auto : .review
                reason = String(format: "Matches your '%@' folder (%.0f%% alike)", category, Double(sim * 100))
            } else {
                // New group named from the cluster's most distinctive tags.
                let terms = distinctiveTerms(members, files: files, globalFreq: globalFreq)
                let base = groupName(fromTerms: terms)
                // Disambiguate a name already claimed by another new-group
                // cluster so distinct clusters get distinct folders (#9). Work in
                // the SANITIZED namespace (#2): two pretty names that differ only
                // in chars componentSafe maps to "_" (e.g. "16:9" and "16/9" →
                // "16_9") must still back two physical directories. (F-C3-013/014)
                var pretty = base
                let safeBase = FilesystemNameSafe.componentSafe(base)
                var safe = safeBase
                // Prefer the next distinctive term first.
                if usedGroupNames.contains(safe), terms.count > 2 {
                    pretty = "\(base) \(titleCase(terms[2]))"
                    safe = FilesystemNameSafe.componentSafe(pretty)
                }
                // Numeric-suffix fallback. Build each candidate so the suffix
                // ALWAYS survives the 200-scalar cap: a base that already
                // sanitizes to ~200 scalars would otherwise truncate every
                // "{base} {n}" to the SAME string, so the uniqueness check never
                // clears and the loop spins forever. Reserve room on the
                // already-sanitized base — distinct n ⇒ distinct candidate ⇒
                // guaranteed termination. (F-C3-014)
                let safeNameMax = 200  // mirrors FilesystemNameSafe default maxLength
                var n = 2
                while usedGroupNames.contains(safe) {
                    let suffix = " \(n)"
                    let room = max(0, safeNameMax - suffix.unicodeScalars.count)
                    let prefix = String(safeBase.unicodeScalars.prefix(room))
                    safe = "\(prefix)\(suffix)"
                    pretty = "\(base) \(n)"
                    n += 1
                }
                usedGroupNames.insert(safe)
                category = pretty
                destDir = (libraryRoot as NSString).appendingPathComponent(safe)
                confidence = (coh >= profile.autoCohesion && members.count >= profile.autoMinMembers)
                    ? .auto : (coh >= profile.reviewCohesion ? .review : .ask)
                if terms.isEmpty {
                    reason = "\(members.count) files that look alike"
                } else {
                    let shown = terms.prefix(3).map { titleCase($0) }.joined(separator: ", ")
                    reason = "\(members.count) files sharing \(shown)"
                }
            }

            for i in members {
                moves.append(Move(
                    fileID: files[i].fileID,
                    source: files[i].source,
                    destinationDir: destDir,
                    category: category,
                    confidence: confidence,
                    reason: reason))
            }
        }
        return moves
    }

    // MARK: - Non-image semantic pass (RESTRUCTURE.md R1)

    /// Cluster non-image files (documents, video, audio — anything without a CLIP
    /// image embedding) by a filename-token + content-tag bag-of-words signature,
    /// so a mixed library groups invoices/manuals/clips by *content* instead of
    /// dumping them all into `Documents/<Year>`. Additive: the image pass runs
    /// first and claims its files, this handles the remainder, and the rule
    /// cascade still catches whatever neither clusters. The bag-of-words IS the
    /// representative vector (there is no image embedding), so the same density
    /// clusterer + learn-your-style folder matching apply unchanged under the
    /// tighter `nonImageProfile`.
    public static func classifyNonImage(
        files: [SemanticFile],
        libraryRoot: String
    ) -> [Move] {
        let sigs = nonImageSignatures(files)
        guard sigs.count >= 2 else { return [] }
        // Learn-your-style targets, but NOT generic dumping grounds (Downloads,
        // Desktop, Temp, …): the whole point is to organize files OUT of those, so
        // they must never become a prototype that routes everything back where it
        // already is. Real user folders ("Taxes", "Invoices") still anchor.
        // (RESTRUCTURE.md R1)
        let protos = folderPrototypes(sigs, minFiles: 4).filter { !isJunkPrototypeFolder($0.path) }
        return classify(files: sigs, prototypes: protos,
                        libraryRoot: libraryRoot, profile: nonImageProfile)
    }

    /// A folder that must never act as a learn-your-style prototype — a generic
    /// dumping ground the butler should organize files *out of*, not route them
    /// back into. Matches the exact junk names AND any folder whose first word is
    /// a dumping-ground word, so versioned/suffixed variants ("Desktop 1.0",
    /// "Downloads (2)", "Temp files") are caught too. (RESTRUCTURE.md R1)
    static func isJunkPrototypeFolder(_ path: String) -> Bool {
        let name = (path as NSString).lastPathComponent.lowercased()
        if junkFolderNames.contains(name) { return true }
        let firstWord = name.split { !$0.isLetter }.first.map(String.init) ?? name
        return junkPrototypePrefixes.contains(firstWord)
    }

    /// Generic dumping grounds — exact basenames the butler never anchors to.
    static let junkFolderNames: Set<String> = [
        "downloads", "downloaded", "desktop", "new folder", "untitled", "temp",
        "tmp", "misc", "other", "stuff", "things", "files", "unsorted", "inbox",
    ]

    /// First-word matches that bar a folder even when suffixed/versioned.
    private static let junkPrototypePrefixes: Set<String> = [
        "desktop", "downloads", "download", "downloaded", "temp", "tmp",
        "unsorted", "inbox", "misc",
    ]

    /// Build bag-of-words representatives for the non-image pass. Each file's
    /// `clip` slot becomes an L2-normalized multi-hot over a shared, frequency-
    /// capped vocab of (filename tokens ∪ content tags); `tags` keeps the same
    /// token set so the distinctive-term namer can still label the group. The
    /// input `clip` is ignored (non-image files have none) — only
    /// `source`/`tags`/`timeUnix` are read. A file with no in-vocab token is
    /// dropped (it has no grouping signal) and falls through to the rule cascade.
    static func nonImageSignatures(_ files: [SemanticFile]) -> [SemanticFile] {
        let tokenSets: [[String]] = files.map { f in
            var set = Set(filenameTokens(f.source))
            for t in f.tags {
                let lt = t.lowercased()
                if !lt.isEmpty { set.insert(lt) }
            }
            return Array(set).sorted()   // deterministic order across runs
        }
        var freq: [String: Int] = [:]
        for toks in tokenSets { for t in toks { freq[t, default: 0] += 1 } }
        let vocab = vocabFromFreq(freq, cap: nonImageVocabCap)
        guard !vocab.isEmpty else { return [] }

        var out: [SemanticFile] = []
        out.reserveCapacity(files.count)
        for (i, f) in files.enumerated() {
            var vec = [Float](repeating: 0, count: vocab.count)
            var any = false
            for t in tokenSets[i] where vocab[t] != nil { vec[vocab[t]!] = 1; any = true }
            guard any else { continue }
            out.append(SemanticFile(fileID: f.fileID, source: f.source,
                                    clip: l2Normalized(vec), tags: tokenSets[i],
                                    timeUnix: f.timeUnix))
        }
        return out
    }

    /// Lowercase alphanumeric filename tokens, extension dropped, split on any
    /// non-alphanumeric. Drops pure-numeric, very short, and generic
    /// camera/scan tokens (no grouping signal): "IMG_4821.heic" → [], but
    /// "acme_invoice_2023.pdf" → ["acme","invoice"]. (RESTRUCTURE.md R1)
    static func filenameTokens(_ path: String) -> [String] {
        let base = (path as NSString).lastPathComponent
        let stem = (base as NSString).deletingPathExtension.lowercased()
        return stem.split { !$0.isLetter && !$0.isNumber }
            .map(String.init)
            .filter { $0.count >= 3 && $0.contains(where: { $0.isLetter })
                && !filenameStopwords.contains($0) }
    }

    /// Filename tokens that carry no grouping signal: camera/scan boilerplate,
    /// common English connectors (so "boys at the zoo" doesn't name a "Boys The"
    /// folder), and file-extension tokens that leak in on double-extension names
    /// like `E14.jpg.lps` → `E14.jpg` → spurious "jpg". Lowercase. (RESTRUCTURE.md R1)
    private static let filenameStopwords: Set<String> = [
        // camera / scan / boilerplate
        "img", "image", "dsc", "dscn", "dscf", "photo", "pic", "picture",
        "screenshot", "screen", "shot", "untitled", "new", "copy", "final",
        "draft", "version", "scan", "document", "file", "video", "vid", "clip",
        // English connectors (no grouping signal)
        "the", "and", "for", "with", "from", "was", "are", "this", "that",
        "your", "our", "his", "her", "its", "out", "all", "has",
        // extension tokens that leak in on double-extension names
        "jpg", "jpeg", "png", "gif", "bmp", "heic", "heif", "tiff", "webp",
        "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf",
        "mov", "mp4", "avi", "mkv", "mp3", "wav", "zip", "rar", "lps",
    ]

    // MARK: - Fusion

    private static func tagFrequencies(_ files: [SemanticFile]) -> [String: Int] {
        var freq: [String: Int] = [:]
        for f in files { for t in f.tags { freq[t, default: 0] += 1 } }
        return freq
    }

    /// Top-`cap` tags by frequency → index map. Common tags carry grouping signal.
    private static func vocabFromFreq(_ freq: [String: Int], cap: Int) -> [String: Int] {
        let ranked = freq.sorted { $0.value != $1.value ? $0.value > $1.value : $0.key < $1.key }
        var vocab: [String: Int] = [:]
        for (i, kv) in ranked.prefix(cap).enumerated() { vocab[kv.key] = i }
        return vocab
    }

    /// Fuse one file: per-block L2-normalize, scale by weight, concatenate, then
    /// L2-normalize the whole so the clusterer's cosine is meaningful.
    private static func fuse(_ file: SemanticFile, vocab: [String: Int], profile: Profile) -> [Float] {
        var out: [Float] = []
        out.reserveCapacity(file.clip.count + vocab.count + 2)

        let clip = l2Normalized(file.clip)
        out.append(contentsOf: clip.map { $0 * profile.wClip })

        var tags = [Float](repeating: 0, count: vocab.count)
        for t in file.tags { if let idx = vocab[t] { tags[idx] = 1 } }
        let tagsN = l2Normalized(tags)
        out.append(contentsOf: tagsN.map { $0 * profile.wTags })

        let (s, c) = dayOfYearCyclical(file.timeUnix)
        out.append(s * profile.wTime)
        out.append(c * profile.wTime)

        return l2Normalized(out)
    }

    /// sin/cos of the day-of-year angle (captures seasonality without raw epoch).
    private static func dayOfYearCyclical(_ timeUnix: Double) -> (Float, Float) {
        guard timeUnix > 0 else { return (0, 0) }
        let day = (Int(timeUnix) / 86_400) % 365
        let angle = 2 * Double.pi * Double(day) / 365
        return (Float(sin(angle)), Float(cos(angle)))
    }

    // MARK: - Clustering (reuse IdentityClustering)

    private static func cluster(_ fused: [[Float]]) -> [Int] {
        let params = fileHyperparams()
        let k = params.kNN
        let n = fused.count
        // R3-12: brute-force cosine kNN below HNSW_MIN; an approximate HNSW index
        // above it, so the O(n²) searcher can't stall the Restructure tab at the
        // documented "tens of thousands of files" scale. Mirrors the Windows
        // engine's restructure_semantic::cluster (HNSW_MIN = 5_000) and reuses the
        // same conversion FaceClustering uses (search returns L2; cosine =
        // 1 − L2²/2 for unit vectors). All fused vectors share one dim, so insert
        // order == node id.
        let hnswMin = 5_000
        let index: HNSWIndex? = {
            // Require UNIFORM dims: HNSWIndex.insert returns -1 WITHOUT appending a
            // node on a dim mismatch, which would shift every later node id and
            // desync the `fused[nID]` neighbor mapping below — silent mis-clustering.
            // A truncated/corrupt blob or a stale different-dim embedding (the loader
            // only checks `count % 4 == 0`, not == dim) can produce a ragged vector,
            // so fall back to the dim-tolerant brute-force path in that case rather
            // than trusting "insert order == node id". (audit R3-12 delta fix)
            guard n >= hnswMin, let dim = fused.first?.count, dim > 0,
                  fused.allSatisfy({ $0.count == dim }) else { return nil }
            let idx = HNSWIndex(dim: dim, M: 16, efConstruction: 200, efSearch: 50)
            for v in fused { _ = idx.insert(v) }
            return idx
        }()
        let result = IdentityClustering.cluster(
            embeddings: fused,
            searcher: { i in
                if let index {
                    let hits = index.search(fused[i], k: k + 1)
                    return hits.compactMap { (rawID, l2dist) -> (neighbor: Int, similarity: Float)? in
                        let nID = Int(rawID)
                        guard nID >= 0, nID < n, nID != i else { return nil }
                        return (neighbor: nID, similarity: 1.0 - (l2dist * l2dist) / 2.0)
                    }
                }
                var hits = (0..<n).compactMap { j -> (neighbor: Int, similarity: Float)? in
                    j == i ? nil : (neighbor: j, similarity: dot(fused[i], fused[j]))
                }
                hits.sort { $0.similarity > $1.similarity }
                return Array(hits.prefix(k))
            },
            params: params)
        return result.clusterIDs
    }

    // MARK: - Learn-your-style assignment

    private static func cohesion(_ memberClip: [[Float]], _ centroid: [Float]) -> Float {
        guard !memberClip.isEmpty else { return 0 }
        let sum = memberClip.reduce(Float(0)) { $0 + dot($1, centroid) }
        return sum / Float(memberClip.count)
    }

    /// Nearest folder prototype + the runner-up similarity, so the caller can
    /// abstain on a low top-1−top-2 margin (RESTRUCTURE.md §4).
    private static func nearestTwoFolders(
        _ centroid: [Float], _ prototypes: [FolderPrototype]
    ) -> (FolderPrototype, Float, Float)? {
        var best: (FolderPrototype, Float)?
        var runnerUp: Float = 0
        for p in prototypes {
            let sim = dot(centroid, p.centroid)
            if let b = best {
                if sim > b.1 { runnerUp = b.1; best = (p, sim) }
                else if sim > runnerUp { runnerUp = sim }
            } else {
                best = (p, sim)
            }
        }
        return best.map { ($0.0, $0.1, runnerUp) }
    }

    /// A cluster's most *distinctive* tags by c-TF-IDF: frequent in the cluster
    /// but rare across the library, so names get specific instead of bland
    /// (RESTRUCTURE.md §5). Ubiquitous tags (in every file → idf 0) drop out.
    private static func distinctiveTerms(
        _ members: [Int], files: [SemanticFile], globalFreq: [String: Int]
    ) -> [String] {
        var inCluster: [String: Int] = [:]
        for i in members { for t in files[i].tags { inCluster[t, default: 0] += 1 } }
        let size = Float(max(members.count, 1))
        let total = Float(max(files.count, 1))
        let scored = inCluster.map { (term, c) -> (String, Float) in
            let tf = Float(c) / size
            let df = Float(globalFreq[term] ?? 1)
            // log takes Double; compute the idf in Double then narrow.
            let idf = Float(max(0, log(Double(total / df))))
            return (term, tf * idf)
        }
        return scored
            .filter { $0.1 > 0 }
            .sorted { $0.1 != $1.1 ? $0.1 > $1.1 : $0.0 < $1.0 }
            .map { $0.0 }
    }

    private static func groupName(fromTerms terms: [String]) -> String {
        let parts = terms.prefix(2).map { titleCase($0) }
        return parts.isEmpty ? "Unsorted" : parts.joined(separator: " ")
    }

    // MARK: - Small numeric + string helpers

    /// Component-wise containment: is `path` the root itself or a descendant of
    /// it? Mirrors Rust `Path::starts_with` (so `/lib2` doesn't match `/lib`).
    /// (F-C3-015)
    static func pathContained(_ path: String, in root: String) -> Bool {
        if path == root { return true }
        let rootPrefix = root.hasSuffix("/") ? root : root + "/"
        return path.hasPrefix(rootPrefix)
    }

    @inline(__always)
    private static func dot(_ a: [Float], _ b: [Float]) -> Float {
        let n = min(a.count, b.count)
        var s: Float = 0
        for i in 0..<n { s += a[i] * b[i] }
        return s
    }

    private static func l2Normalized(_ v: [Float]) -> [Float] {
        var norm: Float = 0
        for x in v { norm += x * x }
        norm = norm.squareRoot()
        guard norm >= 1e-8 else { return v }
        return v.map { $0 / norm }
    }

    private static func meanUnit(_ vecs: [[Float]]) -> [Float]? {
        guard let dim = vecs.first?.count, dim > 0 else { return nil }
        var acc = [Float](repeating: 0, count: dim)
        for v in vecs {
            guard v.count == dim else { return nil }
            for d in 0..<dim { acc[d] += v[d] }
        }
        let inv = 1 / Float(vecs.count)
        for d in 0..<dim { acc[d] *= inv }
        return l2Normalized(acc)
    }

    private static func titleCase(_ s: String) -> String {
        s.split(separator: " ").map { word -> String in
            guard let first = word.first else { return "" }
            return first.uppercased() + String(word.dropFirst())
        }.joined(separator: " ")
    }
}
