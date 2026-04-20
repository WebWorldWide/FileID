import Foundation

/// Monitors a directory for changes using FSEvents.
/// When a new file is added, it notifies the delegate to process it.
class FolderWatcherService {
    static let shared = FolderWatcherService()
    
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
            let watcher = Unmanaged<FolderWatcherService>.fromOpaque(clientCallBackInfo!).takeUnretainedValue()
            let paths = Unmanaged<CFArray>.fromOpaque(eventPaths).takeUnretainedValue() as! [String]
            
            for i in 0..<numEvents {
                let path = paths[i]
                let flag = eventFlags[i]
                
                // Check if it's a new file creation
                if (flag & UInt32(kFSEventStreamEventFlagItemCreated)) != 0 && (flag & UInt32(kFSEventStreamEventFlagItemIsFile)) != 0 {
                    watcher.callback?(URL(fileURLWithPath: path))
                }
            }
        }, &context, pathsToWatch, FSEventStreamEventId(kFSEventStreamEventIdSinceNow), 1.0, flags)
        
        FSEventStreamScheduleWithRunLoop(stream!, CFRunLoopGetCurrent(), CFRunLoopMode.defaultMode.rawValue)
        FSEventStreamStart(stream!)
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
