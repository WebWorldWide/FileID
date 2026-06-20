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
        /// Distinctive filename/folder-name tokens of this folder + its current
        /// contents — the Dropbox "Smart Move" signal that names route as well as
        /// (or better than) content. Used ADDITIVELY: strong name agreement upgrades
        /// a thin-margin content match's confidence, never overrides content routing.
        public let nameTokens: Set<String>
        public init(path: String, centroid: [Float], nameTokens: Set<String> = []) {
            self.path = path
            self.centroid = centroid
            self.nameTokens = nameTokens
        }
    }

    /// Name-routing thresholds (overlap coefficient = |a∩b| / min(|a|,|b|)). At/above
    /// auto, the cluster's filenames agree strongly enough with the target folder to
    /// upgrade a thin-margin content match to Auto; at/above reason the "filenames
    /// fit" note is added. Lockstep with the Rust engine's NAME_AGREE_* constants.
    static let nameAgreeAuto: Float = 0.30
    static let nameAgreeReason: Float = 0.20

    /// Overlap coefficient of two token sets: |a∩b| / min(|a|,|b|), 0 when either is
    /// empty. Less penalized by union size than Jaccard — a cluster that shares a
    /// folder's few distinctive tokens scores high even when each side has extras.
    static func overlapCoefficient(_ a: Set<String>, _ b: Set<String>) -> Float {
        let m = min(a.count, b.count)
        if m == 0 { return 0 }
        return Float(a.intersection(b).count) / Float(m)
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
    // Thresholds calibrated 2026-06-17 against a real ~3.3k-image personal-photo library
    // (the "Adlon" corpus). Finding: CLIP cosines for personal photos compress into a HIGH
    // band — intra-folder cohesion median ≈ 0.80, inter-folder centroid p90 ≈ 0.84 — so the
    // original folderMatchCos 0.55 / autoFolderCos 0.72 sat BELOW the entire distribution
    // and auto-routed every photo into the nearest catch-all folder (109 event folders →
    // one "Camera Roll"). The new bar sits between the inter-folder p90 (don't merge across
    // events) and the intra-folder median (still group a real event). Env-overridable for
    // further owner tuning, mirroring the non-image knobs. (RESTRUCTURE.md R3 calibration)
    public static var imageProfile: Profile {
        Profile(
            wClip: 0.70, wTags: 0.22, wTime: 0.08,
            folderMatchCos: envFloat("FILEID_RESTRUCTURE_IMG_FOLDER_COS", 0.80),
            autoFolderCos: envFloat("FILEID_RESTRUCTURE_IMG_AUTO_FOLDER_COS", 0.86),
            autoCohesion: envFloat("FILEID_RESTRUCTURE_IMG_AUTO_COH", 0.78),
            reviewCohesion: envFloat("FILEID_RESTRUCTURE_IMG_REVIEW_COH", 0.70),
            minMargin: 0.05, autoMinMembers: 4)
    }

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
        // Cluster-merge cosines calibrated 2026-06-17 on the real ~3.3k-image Adlon
        // corpus. The original 0.50/0.40/0.42 were tuned for DIVERSE images; CLIP cosines
        // for a coherent personal library compress into a high band (typical pair ≈ 0.71+,
        // within-event ≈ 0.80), so those low bars merged the ENTIRE photo set into one
        // cluster that then routed to a single catch-all folder. The new bars sit at the
        // within-event cohesion so a cluster ≈ one event. Env-overridable for owner tuning;
        // the single-knob GRANULARITY delta still shifts all three together.
        let d = granularityDelta()
        return IdentityClustering.Hyperparameters(
            pass1Cosine: envFloat("FILEID_RESTRUCTURE_CLUSTER_P1", 0.84) + d,
            pass2Cosine: envFloat("FILEID_RESTRUCTURE_CLUSTER_P2", 0.76) + d,
            pass2Margin: 0.08 + d * 0.5,  // delta scales margin proportionally
            pass3VarianceThreshold: 0.06,
            pass3MinMeanCosine: envFloat("FILEID_RESTRUCTURE_CLUSTER_P3", 0.76) + d,
            pass3MaxSplits: 5, kNN: 12)
    }

    /// Build prototypes from the files' *current* locations: each parent folder
    /// with ≥ `minFiles` becomes a class whose centroid is the mean CLIP vector
    /// of its contents (Nearest-Class-Mean / Dropbox "Smart Move"). Zero user
    /// effort — the existing tree is the labeled ground truth.
    public static func folderPrototypes(_ files: [SemanticFile], minFiles: Int) -> [FolderPrototype] {
        var byFolder: [String: [SemanticFile]] = [:]
        for f in files {
            let parent = (f.source as NSString).deletingLastPathComponent
            byFolder[parent, default: []].append(f)
        }
        var out: [FolderPrototype] = []
        for (path, fs) in byFolder where fs.count >= minFiles {
            if let centroid = meanUnit(fs.map { $0.clip }) {
                // Folder's own name tokens + every sibling filename's tokens.
                var nameTokens = Set(filenameTokens(path))
                for f in fs { nameTokens.formUnion(filenameTokens(f.source)) }
                out.append(FolderPrototype(path: path, centroid: centroid, nameTokens: nameTokens))
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
    ///
    /// The image pass pre-segments files by capture-time gap before clustering so
    /// events separated by > `FILEID_RESTRUCTURE_TIME_GAP` seconds (default 2 h)
    /// never compete in the same cluster. Time gaps are the strongest natural
    /// album-boundary signal; CLIP handles within-event appearance separation.
    public static func classify(
        files: [SemanticFile],
        prototypes: [FolderPrototype],
        libraryRoot: String,
        profile: Profile = imageProfile
    ) -> [Move] {
        let hp = fileHyperparams()
        // Shared name registry created ONCE here and threaded through every
        // segment, so segment 1's "Beach" cluster doesn't collide with segment
        // 2's independent "Beach" cluster into the same folder — names stay
        // globally unique across the whole reorg. (BL-01)
        var usedGroupNames = Set<String>()
        // Non-default profile (document / non-image pass): skip time segmentation —
        // docs lack reliable timestamps and the passage handles them separately.
        guard profile.wTime == imageProfile.wTime && profile.wClip == imageProfile.wClip else {
            return classifyProfiled(files: files, prototypes: prototypes,
                                    libraryRoot: libraryRoot, profile: profile, hp: hp,
                                    usedGroupNames: &usedGroupNames)
        }
        var moves: [Move] = []
        for segment in timeSegments(files) {
            moves.append(contentsOf: classifyProfiled(
                files: segment, prototypes: prototypes,
                libraryRoot: libraryRoot, profile: profile, hp: hp,
                usedGroupNames: &usedGroupNames))
        }
        return moves
    }

    /// Default time-gap threshold: 2 hours between consecutive photos signals a
    /// new event. Configurable via `FILEID_RESTRUCTURE_TIME_GAP` (seconds).
    static func timeGapSeconds() -> Double {
        ProcessInfo.processInfo.environment["FILEID_RESTRUCTURE_TIME_GAP"]
            .flatMap(Double.init) ?? 7_200
    }

    /// Pre-segment photos by capture-time gap. Files separated by > `timeGapSeconds()`
    /// go into independent segments that are clustered on their own — time gaps are
    /// the primary event-boundary signal. Files without a timestamp cluster last.
    /// (RESTRUCTURE.md §2 — "time-gap event segmentation cascade")
    static func timeSegments(_ files: [SemanticFile]) -> [[SemanticFile]] {
        guard files.count >= 2 else { return [files] }
        let gap = timeGapSeconds()
        let timed = files.filter { $0.timeUnix > 0 }.sorted { $0.timeUnix < $1.timeUnix }
        let untimed = files.filter { $0.timeUnix <= 0 }
        var segments: [[SemanticFile]] = []
        var cur: [SemanticFile] = []
        for f in timed {
            if let last = cur.last, f.timeUnix - last.timeUnix > gap {
                segments.append(cur)
                cur = [f]
            } else { cur.append(f) }
        }
        if !cur.isEmpty { segments.append(cur) }
        if !untimed.isEmpty { segments.append(untimed) }
        return segments
    }

    static func classifyProfiled(
        files: [SemanticFile],
        prototypes: [FolderPrototype],
        libraryRoot: String,
        profile: Profile,
        hp: IdentityClustering.Hyperparameters,
        usedGroupNames: inout Set<String>
    ) -> [Move] {
        guard !files.isEmpty else { return [] }
        let globalFreq = tagFrequencies(files)
        let vocab = vocabFromFreq(globalFreq, cap: tagVocabCap)
        let fused = files.map { fuse($0, vocab: vocab, profile: profile) }
        let clusterIDs = cluster(fused, hp)

        var clusters: [Int: [Int]] = [:]
        for (i, cid) in clusterIDs.enumerated() { clusters[cid, default: []].append(i) }

        var moves: [Move] = []
        // `usedGroupNames` (an inout param) is the shared registry of names
        // already claimed by a *different* new-group cluster — this run AND
        // every prior time-segment, so two segments that both mint "Beach" get
        // distinct folders (BL-01, #9). Consulted ONLY by the new-group branch;
        // the existing-folder branch legitimately routes many clusters into one
        // user folder. Tracked in the SANITIZED namespace that actually backs
        // the directory. (F-C3-014)
        // Stable cluster iteration (smallest id first) so the dedup below is
        // deterministic across runs.
        for cid in clusters.keys.sorted() {
            let members = clusters[cid]!
            // Singletons (the clusterer's outliers) have no group signal.
            guard members.count >= 2 else { continue }
            let memberClip = members.map { files[$0].clip }
            guard let centroid = meanUnit(memberClip) else { continue }
            let coh = cohesion(memberClip, centroid)
            // Distinctive filename tokens shared by this cluster — the name-routing
            // signal matched against each candidate folder below.
            let clusterNameTokens = members.reduce(into: Set<String>()) {
                $0.formUnion(filenameTokens(files[$1].source))
            }

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
                // Name-routing (Dropbox Smart Move): additive — strong filename
                // agreement upgrades a thin-margin content match to Auto, but never
                // overrides the content routing decision itself.
                let nameSim = Self.overlapCoefficient(clusterNameTokens, proto.nameTokens)
                let contentAuto = sim >= profile.autoFolderCos && coh >= profile.reviewCohesion && (sim - runnerUp) >= profile.minMargin
                let nameAuto = nameSim >= Self.nameAgreeAuto && sim >= profile.folderMatchCos && coh >= profile.reviewCohesion
                confidence = (contentAuto || nameAuto) ? .auto : .review
                reason = nameSim >= Self.nameAgreeReason
                    ? String(format: "Matches your '%@' folder (%.0f%% alike; the filenames fit too)", category, Double(sim * 100))
                    : String(format: "Matches your '%@' folder (%.0f%% alike)", category, Double(sim * 100))
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
        var usedGroupNames = Set<String>()
        return classifyProfiled(files: sigs, prototypes: protos,
                                libraryRoot: libraryRoot, profile: nonImageProfile,
                                hp: fileHyperparams(), usedGroupNames: &usedGroupNames)
    }

    /// Document-content pass — cluster documents by their BGE text embedding (in `clip`),
    /// which reads the content rather than the filename. Mirrors the Windows engine's
    /// `classify_documents`. Each `file.clip` MUST be the 384-d BGE vector; the caller
    /// supplies only docs it could extract text for + embed. Doc-specific thresholds
    /// because BGE cosines sit lower than CLIP-image cosines. (RESTRUCTURE.md R3)
    public static func classifyDocuments(
        files: [SemanticFile],
        libraryRoot: String
    ) -> [Move] {
        guard files.count >= 2 else { return [] }
        let protos = folderPrototypes(files, minFiles: 4).filter { !isJunkPrototypeFolder($0.path) }
        var usedGroupNames = Set<String>()
        return classifyProfiled(files: files, prototypes: protos,
                                libraryRoot: libraryRoot, profile: docProfile,
                                hp: docHyperparams(), usedGroupNames: &usedGroupNames)
    }

    /// Document content-embedding profile (byte-faithful with Rust `doc_profile`). The
    /// representative IS the 384-d BGE vector (`wClip` dominates). Thresholds CALIBRATED
    /// 2026-06-17 on the owner's real ~1.4k-doc corpus: the engine MEAN-pools BGE, whose
    /// cosines compress high (within-folder cohesion ≈ 0.786, inter p90 ≈ 0.80), so the bars
    /// sit there — NOT at the lower CLS-pooled A/B values, which collapsed docs into one
    /// folder. Validated: doc folder-agreement 46%→53%. Env-overridable.
    static var docProfile: Profile {
        Profile(
            wClip: 0.92, wTags: 0.06, wTime: 0.02,
            folderMatchCos: envFloat("FILEID_RESTRUCTURE_DOC_FOLDER_COS", 0.78),
            autoFolderCos: envFloat("FILEID_RESTRUCTURE_DOC_AUTO_FOLDER_COS", 0.84),
            autoCohesion: envFloat("FILEID_RESTRUCTURE_DOC_AUTO_COH", 0.78),
            reviewCohesion: envFloat("FILEID_RESTRUCTURE_DOC_REVIEW_COH", 0.70),
            minMargin: 0.05, autoMinMembers: 4)
    }

    /// Cluster-merge cosines for the MEAN-pooled BGE document space (compresses high, like
    /// the image space). Byte-faithful with Rust `doc_hyperparams`. Env-overridable.
    static func docHyperparams() -> IdentityClustering.Hyperparameters {
        let d = granularityDelta()
        return IdentityClustering.Hyperparameters(
            pass1Cosine: envFloat("FILEID_RESTRUCTURE_DOC_CLUSTER_P1", 0.82) + d,
            pass2Cosine: envFloat("FILEID_RESTRUCTURE_DOC_CLUSTER_P2", 0.74) + d,
            pass2Margin: 0.06,
            pass3VarianceThreshold: 0.06,
            pass3MinMeanCosine: envFloat("FILEID_RESTRUCTURE_DOC_CLUSTER_P3", 0.74) + d,
            pass3MaxSplits: 5, kNN: 12)
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
            // A file whose every token is unique to it (each token freq 1) shares
            // NO signal with any other file, so it's orthogonal to all of them and
            // can never cluster — leave it for the rule cascade. Excluding it up
            // front (instead of trusting the density clusterer to noise-reject an
            // orthogonal point) keeps the result deterministic across architectures:
            // with k_nn >= n and all-tied zero similarities, the clusterer's kNN
            // tie order is arch-sensitive, which made a degenerate lone file
            // group-or-not by luck. (CI determinism / lockstep)
            guard tokenSets[i].contains(where: { (freq[$0] ?? 0) >= 2 }) else { continue }
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
            // Length is measured in Unicode SCALARS, not grapheme clusters, to match
            // the Rust engine's `chars().count()` — a String's `.count` counts
            // graphemes, so an NFD multi-scalar single-grapheme token (e.g. a decomposed
            // Hangul syllable) would pass the ≥3 gate on Windows but fail it on macOS,
            // diverging the token set used for feedback + name-routing. (audit — lockstep)
            .filter { $0.unicodeScalars.count >= 3 && $0.contains(where: { $0.isLetter })
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
        out.reserveCapacity(file.clip.count + vocab.count + 5)

        let clip = l2Normalized(file.clip)
        out.append(contentsOf: clip.map { $0 * profile.wClip })

        var tags = [Float](repeating: 0, count: vocab.count)
        for t in file.tags { if let idx = vocab[t] { tags[idx] = 1 } }
        let tagsN = l2Normalized(tags)
        out.append(contentsOf: tagsN.map { $0 * profile.wTags })

        let tf = timeFeatures(file.timeUnix)
        for v in tf { out.append(v * profile.wTime) }

        return l2Normalized(out)
    }

    /// Five time features: day-of-year cyclical (2), time-of-day cyclical (2),
    /// log-compressed absolute year (1). Together they separate:
    /// - events in different seasons (day-of-year cyclical)
    /// - morning vs evening sessions (time-of-day cyclical)
    /// - same calendar day in different years (log-year, monotonic)
    /// Byte-faithful with the Rust engine. (RESTRUCTURE.md §2)
    static func timeFeatures(_ timeUnix: Double) -> [Float] {
        guard timeUnix > 0 else { return [Float](repeating: 0, count: 5) }
        let secs = Int64(timeUnix)
        let day = Int((secs / 86_400) % 365)
        let dayAngle = 2 * Double.pi * Double(day) / 365
        let secondOfDay = Int(secs % 86_400)
        let todAngle = 2 * Double.pi * Double(secondOfDay) / 86_400
        // log1p(years) / log1p(100): monotonic 0→1 over a 100-year range;
        // separates same-calendar-day events in different years.
        let years = max(0, min(100, timeUnix / (365.25 * 86_400)))
        let logYear = Float(log(1 + years) / log(101))
        return [Float(sin(dayAngle)), Float(cos(dayAngle)),
                Float(sin(todAngle)), Float(cos(todAngle)), logYear]
    }

    // MARK: - Clustering (reuse IdentityClustering)

    private static func cluster(_ fused: [[Float]], _ params: IdentityClustering.Hyperparameters) -> [Int] {
        let n = fused.count
        // Can't request more neighbors than other points exist; k_nn >= n made the
        // kNN over an all-tied set arch-sensitive (see nonImageSignatures). (lockstep)
        let k = min(params.kNN, max(1, n - 1))
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
