import Foundation

/// Monitors a directory for changes using FSEvents.
/// When a new file is added, it notifies the delegate to process it.
class FolderWatcherService {
    nonisolated(unsafe) static let shared = FolderWatcherService()
    
    private var stream: FSEventStreamRef?
    private var callback: ((URL) -> Void)?
    
    func startWatching(url: URL, onFileAdded: @escaping (URL) -> Void) {
        stopWatching()
        self.callback = onFileAdded
        
        let path = url.path as NSString
        let pathsToWatch = [path] as CFArray
        var context = FSEventStreamContext(version: 0, info: UnsafeMutableRawPointer(Unmanaged.passUnretained(self).toOpaque()), retain: nil, release: nil, copyDescription: nil)
        
        let flags = UInt32(kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer)
        
        stream = FSEventStreamCreate(nil, { (streamRef, clientCallBackInfo, numEvents, eventPaths, eventFlags, eventIds) in
            guard let info = clientCallBackInfo else { return }
            // Singleton lifetime guarantees this pointer is valid. Assert identity
            // so any future non-singleton caller trips the check instead of reading
            // dangling memory — the `passUnretained` on a non-singleton instance
            // would be a use-after-free waiting to happen.
            let watcher = Unmanaged<FolderWatcherService>.fromOpaque(info).takeUnretainedValue()
            assert(watcher === FolderWatcherService.shared, "FolderWatcherService.startWatching: only the shared singleton is callback-safe")
            guard let paths = Unmanaged<CFArray>.fromOpaque(eventPaths).takeUnretainedValue() as? [String] else { return }

            for i in 0..<numEvents {
                let path = paths[i]
                let flag = eventFlags[i]
                if (flag & UInt32(kFSEventStreamEventFlagItemCreated)) != 0
                    && (flag & UInt32(kFSEventStreamEventFlagItemIsFile)) != 0 {
                    watcher.callback?(URL(fileURLWithPath: path))
                }
            }
        }, &context, pathsToWatch, FSEventStreamEventId(kFSEventStreamEventIdSinceNow), 1.0, flags)

        guard let s = stream else { return }
        FSEventStreamSetDispatchQueue(s, DispatchQueue.global(qos: .userInitiated))
        FSEventStreamStart(s)
    }
    
    func stopWatching() {
        if let stream = stream {
            FSEventStreamStop(stream)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
            self.stream = nil
        }
    }
}
