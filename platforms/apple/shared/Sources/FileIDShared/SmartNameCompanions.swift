import Foundation

/// Files that keep one name stem when smart names rename them: a RAW and its
/// JPEG/HEIC, a Live Photo's HEIC/JPEG and MOV, and `.xmp`/`.aae` sidecars
/// (`IMG_1.xmp` or darktable's `IMG_1.ARW.xmp`). Lockstep with Rust
/// `util/smart_name_companions.rs`; names are compared per Unicode scalar.
public enum SmartNameCompanions {
    typealias S = SmartNameStyle

    public enum Role: Int, Sendable, Comparable {
        case image, raw, motion, sidecar
        public static func < (a: Role, b: Role) -> Bool { a.rawValue < b.rawValue }
    }

    static let raw: Set<String> = [
        "3fr", "arw", "cr2", "cr3", "dng", "erf", "iiq", "kdc", "mef", "mos", "mrw", "nef", "nrw", "orf", "pef",
        "raf", "raw", "rw2", "rwl", "srf", "srw", "x3f",
    ]

    public static let unscanned = "unscannedCompanion"

    static func asciiLower(_ text: String) -> String {
        S.string(text.unicodeScalars.map { ("A"..."Z").contains($0) ? Unicode.Scalar($0.value + 32)! : $0 })
    }

    public static func role(_ ext: String) -> Role? {
        let ext = asciiLower(ext)
        switch ext {
        case "jpg", "jpeg", "heic", "heif": return .image
        case "mov": return .motion
        case "xmp", "aae": return .sidecar
        default: return raw.contains(ext) ? .raw : nil
        }
    }

    static func base(_ name: String) -> (stem: String, suffix: String, role: Role)? {
        guard name.unicodeScalars.first != "." else { return nil }
        var (stem, ext) = S.splitName(name)
        guard let role = role(ext) else { return nil }
        if role == .sidecar {
            let (inner, innerExt) = S.splitName(stem)
            if let innerRole = self.role(innerExt), innerRole != .sidecar { stem = inner }
        }
        let suffix = S.string(Array(name.unicodeScalars.dropFirst(stem.unicodeScalars.count)))
        return (stem, suffix, role)
    }

    public struct Member: Sendable, Equatable {
        public let name: String
        public let suffix: String
        public let role: Role
    }

    /// `name` and every companion of it in `listing`, in role then name order;
    /// empty when it has none.
    public static func members(_ name: String, listing: [String]) -> [Member] {
        guard let mine = base(name) else { return [] }
        let key = S.key(S.lower(mine.stem))
        var found = listing.compactMap { candidate -> Member? in
            guard let other = base(candidate), S.key(S.lower(other.stem)) == key else { return nil }
            return Member(name: candidate, suffix: other.suffix, role: other.role)
        }
        if !found.contains(where: { S.key($0.name) == S.key(name) }) {
            found.append(Member(name: name, suffix: mine.suffix, role: mine.role))
        }
        found.sort { a, b in a.role != b.role ? a.role < b.role : a.name.unicodeScalars.lexicographicallyPrecedes(b.name.unicodeScalars) }
        var unique: [Member] = []
        for member in found where unique.last.map({ S.key($0.name) }) != S.key(member.name) { unique.append(member) }
        return isGroup(unique) ? unique : []
    }

    public static func isGroup(_ members: [Member]) -> Bool {
        members.count >= 2 && members.contains { $0.role != .sidecar }
    }

    /// The first `_N` bump (1 = none, up to 99) at which every member's new
    /// name, `stem` plus its own suffix, is free in the folder and the batch,
    /// or its own name in another case. `members` holds each member's suffix
    /// and lowercased current name.
    public static func sharedNames(stem: String, members: [(suffix: String, own: SmartNameStyle.Key)], taken: Set<SmartNameStyle.Key>, claimed: Set<SmartNameStyle.Key>) -> [String]? {
        for bump in 1...99 {
            let base = bump == 1 ? stem : "\(stem)_\(bump)"
            let names = members.map { base + $0.suffix }
            var keys = Set<S.Key>()
            let free = zip(names, members).allSatisfy { name, member in
                let k = S.key(S.lower(name))
                return keys.insert(k).inserted && (k == member.own || (!taken.contains(k) && !claimed.contains(k)))
            }
            if free { return names }
        }
        return nil
    }

    /// Lower is better evidence, in the order `SmartNameStyle.propose` reads its sources.
    public static func sourceRank(_ source: String, style: SmartNameStyle.Style) -> Int {
        switch source {
        case "chapter": return 0
        case "description" where style == .descriptive, "proposedName" where style != .descriptive: return 1
        case "description", "proposedName": return 2
        case "tags": return 3
        case "event": return 4
        default: return 5
        }
    }

    public static func groupReason(_ reason: String, leader: Int64) -> String { "\(reason)|group:\(leader)" }

    /// Splits a proposeNames `evidenceID` into its reason and the group
    /// leader's file ID, if the file renames with companions.
    public static func parse(_ evidenceID: String?) -> (reason: String, group: Int64?) {
        let text = evidenceID ?? "noEvidence"
        guard let marker = text.range(of: "|group:") else { return (text, nil) }
        return (String(text[..<marker.lowerBound]), Int64(text[marker.upperBound...]))
    }
}
