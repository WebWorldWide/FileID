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

    // Fusion weights (RESTRUCTURE.md §2): content embedding dominates; tags
    // refine; time nudges. Each block is L2-normalized before weighting.
    private static let wClip: Float = 0.70
    private static let wTags: Float = 0.22
    private static let wTime: Float = 0.08
    private static let tagVocabCap = 256

    // Route a cluster into an existing folder above this cosine; below, a new
    // group. Confidence-band thresholds match the Rust engine (provisional —
    // calibrate to measured per-category accuracy before standing auto-file).
    private static let folderMatchCos: Float = 0.55
    private static let autoFolderCos: Float = 0.72
    private static let autoCohesion: Float = 0.62
    private static let reviewCohesion: Float = 0.50
    private static let minMargin: Float = 0.05
    private static let autoMinMembers = 4

    private static func fileHyperparams() -> IdentityClustering.Hyperparameters {
        // Looser than faces: a semantic group is broader than one identity.
        IdentityClustering.Hyperparameters(
            pass1Cosine: 0.50, pass2Cosine: 0.40, pass2Margin: 0.08,
            pass3VarianceThreshold: 0.06, pass3MinMeanCosine: 0.42,
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
        libraryRoot: String
    ) -> [Move] {
        guard !files.isEmpty else { return [] }
        let globalFreq = tagFrequencies(files)
        let vocab = vocabFromFreq(globalFreq, cap: tagVocabCap)
        let fused = files.map { fuse($0, vocab: vocab) }
        let clusterIDs = cluster(fused)

        var clusters: [Int: [Int]] = [:]
        for (i, cid) in clusterIDs.enumerated() { clusters[cid, default: []].append(i) }

        var moves: [Move] = []
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

            if let (proto, sim, runnerUp) = nearestTwoFolders(centroid, prototypes), sim >= folderMatchCos {
                // Learn-your-style: route to the nearest confident existing
                // folder. Auto only when strong AND unambiguous on a tight cluster.
                let name = (proto.path as NSString).lastPathComponent
                category = name.isEmpty ? "Folder" : name
                destDir = proto.path
                confidence = (sim >= autoFolderCos && coh >= reviewCohesion && (sim - runnerUp) >= minMargin)
                    ? .auto : .review
                reason = String(format: "Matches your '%@' folder (%.0f%% alike)", category, Double(sim * 100))
            } else {
                // New group named from the cluster's most distinctive tags.
                let terms = distinctiveTerms(members, files: files, globalFreq: globalFreq)
                let name = groupName(fromTerms: terms)
                category = name
                destDir = (libraryRoot as NSString).appendingPathComponent(name)
                confidence = (coh >= autoCohesion && members.count >= autoMinMembers)
                    ? .auto : (coh >= reviewCohesion ? .review : .ask)
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
    private static func fuse(_ file: SemanticFile, vocab: [String: Int]) -> [Float] {
        var out: [Float] = []
        out.reserveCapacity(file.clip.count + vocab.count + 2)

        let clip = l2Normalized(file.clip)
        out.append(contentsOf: clip.map { $0 * wClip })

        var tags = [Float](repeating: 0, count: vocab.count)
        for t in file.tags { if let idx = vocab[t] { tags[idx] = 1 } }
        let tagsN = l2Normalized(tags)
        out.append(contentsOf: tagsN.map { $0 * wTags })

        let (s, c) = dayOfYearCyclical(file.timeUnix)
        out.append(s * wTime)
        out.append(c * wTime)

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
        // Brute-force cosine kNN (mirrors the Rust < HNSW_MIN path). An HNSW
        // index is the upgrade for very large libraries (Windows uses one
        // above 5k files).
        let result = IdentityClustering.cluster(
            embeddings: fused,
            searcher: { i in
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
