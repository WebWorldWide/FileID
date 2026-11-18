// Maps arbitrary strings to filename components safe on Windows NTFS, Linux and BSD, for names that may sync to a non-macOS filesystem.
import Foundation

public enum FilesystemNameSafe {
    private static let illegalChars: Set<Character> = [
        "<", ">", ":", "\"", "/", "\\", "|", "?", "*",
    ]
    private static let reservedBaseNames: Set<String> = [
        "con", "prn", "aux", "nul",
        "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
        "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ]

    /// Returns a Windows + Linux + BSD safe filename component:
    /// - Replaces `<>:"/\|?*` and ASCII 0–31 with `_`
    /// - Trims trailing dots and spaces (Windows strips them)
    /// - Suffixes `_` to Windows reserved basenames (CON, PRN, …)
    /// - Caps at `maxLength` (default 200; Windows component limit is 255,
    ///   leaving headroom for parent paths)
    /// - Empty input returns `_` so callers never get an empty component.
    public static func componentSafe(_ raw: String, maxLength: Int = 200) -> String {
        var out = String()
        out.reserveCapacity(raw.count)
        for scalar in raw.unicodeScalars {
            let value = scalar.value
            if value < 32 {
                out.append("_")
            } else if illegalChars.contains(Character(scalar)) {
                out.append("_")
            } else {
                out.unicodeScalars.append(scalar)
            }
        }
        out = String(out.unicodeScalars.prefix(maxLength))
        while let last = out.last, last == "." || last == " " {
            out.removeLast()
        }
        if out.isEmpty { return "_" }
        let basename: String = {
            if let dot = out.firstIndex(of: ".") {
                return String(out[..<dot]).lowercased()
            }
            return out.lowercased()
        }()
        if reservedBaseNames.contains(basename) {
            out = "_" + out
        }
        return out
    }
}
