import Foundation

/// The engine keeps the last Organize as `restructure_undo.ndjson` in its state folder: a header line naming the library folder, then one line per move. A journal with moves is a layout still waiting to be undone, and the app has to know that after a relaunch.
enum RestructureUndoJournal {
    static let fileName = "restructure_undo.ndjson"

    static var stateFolder: URL {
        ReadStore.defaultDBURL.deletingLastPathComponent()
    }

    /// The folder whose Organize is waiting to be undone, or nil when there is no journal or it holds no moves.
    static func pendingRoot(in folder: URL = stateFolder) -> String? {
        guard let handle = try? FileHandle(forReadingFrom: folder.appendingPathComponent(fileName)) else { return nil }
        defer { try? handle.close() }
        guard let head = try? handle.read(upToCount: 64 * 1024), !head.isEmpty else { return nil }
        let lines = head.split(separator: UInt8(ascii: "\n"), omittingEmptySubsequences: true)
        guard lines.count >= 2,
              let header = try? JSONSerialization.jsonObject(with: Data(lines[0])) as? [String: Any],
              let root = header["library_root"] as? String, !root.isEmpty
        else { return nil }
        return root
    }
}
