import CoreServices
import Foundation

/// Asks FSEvents whether files under a folder changed after a point in time, so a
/// scan can catch files added, renamed, edited or deleted while it ran. Reading the
/// history is a few milliseconds when nothing changed.
enum FileSystemChanges {
    static func currentEventID() -> FSEventStreamEventId { FSEventsGetCurrentEventId() }

    private static let contentFlags = FSEventStreamEventFlags(
        kFSEventStreamEventFlagItemCreated | kFSEventStreamEventFlagItemRemoved
        | kFSEventStreamEventFlagItemRenamed | kFSEventStreamEventFlagItemModified
        | kFSEventStreamEventFlagMustScanSubDirs | kFSEventStreamEventFlagRootChanged)

    private final class Collector: @unchecked Sendable {
        let prefix: String
        let root: String
        let done = DispatchSemaphore(value: 0)
        private let lock = NSLock()
        private var found = false
        private var finished = false

        init(root: String) {
            self.root = root
            prefix = root.hasSuffix("/") ? root : root + "/"
        }

        func record(path: String, flags: FSEventStreamEventFlags) {
            if flags & FSEventStreamEventFlags(kFSEventStreamEventFlagHistoryDone) != 0 {
                lock.lock()
                let first = !finished
                finished = true
                lock.unlock()
                if first { done.signal() }
                return
            }
            guard flags & FileSystemChanges.contentFlags != 0,
                  path == root || path.hasPrefix(prefix) || root.hasPrefix(path.hasSuffix("/") ? path : path + "/"),
                  !path.dropFirst(prefix.count).split(separator: "/").contains(where: { $0.hasPrefix(".") }) else { return }
            lock.lock(); found = true; lock.unlock()
        }

        var changed: Bool { lock.lock(); defer { lock.unlock() }; return found }
    }

    /// True when FSEvents recorded a change to a visible item under `root` after
    /// `since`; nil when the history could not be read in time.
    static func changed(under root: URL, since: FSEventStreamEventId, timeout: TimeInterval = 3) -> Bool? {
        // realpath, not resolvingSymlinksInPath: Foundation strips "/private" from
        // /private/tmp and /private/var, but FSEvents reports the real path.
        guard let resolved = realpath(root.path, nil) else { return nil }
        let path = String(cString: resolved)
        free(resolved)
        let collector = Collector(root: path)
        var context = FSEventStreamContext(version: 0, info: Unmanaged.passUnretained(collector).toOpaque(),
                                           retain: nil, release: nil, copyDescription: nil)
        let callback: FSEventStreamCallback = { _, info, count, paths, flags, _ in
            guard let info else { return }
            let collector = Unmanaged<Collector>.fromOpaque(info).takeUnretainedValue()
            let list = Unmanaged<CFArray>.fromOpaque(paths).takeUnretainedValue() as? [String] ?? []
            for index in 0..<min(count, list.count) { collector.record(path: list[index], flags: flags[index]) }
        }
        let flags = FSEventStreamCreateFlags(kFSEventStreamCreateFlagNoDefer | kFSEventStreamCreateFlagFileEvents
                                             | kFSEventStreamCreateFlagUseCFTypes)
        guard let stream = FSEventStreamCreate(kCFAllocatorDefault, callback, &context, [path] as CFArray,
                                               since, 0, flags) else { return nil }
        let queue = DispatchQueue(label: "com.fileid.scan-changes")
        FSEventStreamSetDispatchQueue(stream, queue)
        guard FSEventStreamStart(stream) else {
            FSEventStreamInvalidate(stream); FSEventStreamRelease(stream)
            return nil
        }
        let finished = collector.done.wait(timeout: .now() + timeout) == .success
        FSEventStreamStop(stream)
        FSEventStreamInvalidate(stream)
        queue.sync {}
        FSEventStreamRelease(stream)
        return finished ? collector.changed : nil
    }
}
