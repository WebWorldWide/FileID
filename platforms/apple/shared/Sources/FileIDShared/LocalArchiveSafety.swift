import Foundation

public enum LocalArchiveSafety {
    public static func entryNameIsSafe(_ name: String) -> Bool {
        let components = name.split(separator: "/", omittingEmptySubsequences: false)
        return name.utf8.count <= 4_096
            && !name.hasPrefix("/")
            && !name.hasPrefix("\\")
            && !name.contains("\\")
            && !name.contains("\0")
            && !components.contains(where: { $0 == ".." })
            && !(name.count >= 2 && name[name.index(after: name.startIndex)] == ":")
    }

    public static func unixEntryTypeIsSafe(_ permissions: String) -> Bool {
        permissions.first == "-" || permissions.first == "d"
    }

    public static func fileSecurityStatusIsUnencrypted(_ value: String) -> Bool {
        value.trimmingCharacters(in: .whitespacesAndNewlines)
            .caseInsensitiveCompare("not encrypted") == .orderedSame
    }
}
