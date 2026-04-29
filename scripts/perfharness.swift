// Perf harness — spawns FileIDEngine, runs a scan against /Volumes/Adlon/TrueNAS
// for up to N seconds, captures batch-summary events, reports throughput.
import Foundation

struct IPCCommand: Codable {
    let id: String; let payload: Payload
    enum Payload: Codable {
        case startScan(rootBookmark: Data, rootPathDisplay: String)
        case pauseScan, resumeScan, cancelScan, requestStatus, shutdown
    }
}
struct IPCEvent: Codable {
    let t: Date; let payload: Payload
    enum Payload: Codable {
        case ready(EngineInfo)
        case progress(ScanProgress)
        case phaseChanged(String)
        case discoveryComplete(totalFiles: Int)
        case fileDone(Empty); case batchSummary(BatchSummary); case scanComplete(ScanComplete)
        case error(EngineError); case log(Empty)
    }
}
struct EngineInfo: Codable { let version: String; let pid: Int32; let workerCap: Int; let physicalMemoryGB: Double }
struct ScanProgress: Codable {
    let sessionID: String; let phase: String; let total: Int
    let discovered: Int; let processed: Int; let failed: Int
    let filesPerSecond: Double; let etaSeconds: Double?
    let residentMB: Int; let availableMB: Int
}
struct BatchSummary: Codable {
    let batchIndex: Int; let filesInBatch: Int; let processedTotal: Int
    let wallSeconds: Double; let filesPerSecond: Double; let utilization: Double
    let visionP50Ms: Double; let visionP95Ms: Double
    let clipP50Ms: Double; let clipP95Ms: Double
    let storeInsertP50Ms: Double; let storeInsertP95Ms: Double
    let residentMB: Int; let availableMB: Int
}
struct ScanComplete: Codable { let sessionID: String; let totalFiles: Int; let processedFiles: Int; let failedFiles: Int; let totalSeconds: Double }
struct EngineError: Codable { let kind: String; let message: String; let path: String? }
struct Empty: Codable {}

let runSeconds: Double = CommandLine.arguments.count > 1
    ? Double(CommandLine.arguments[1]) ?? 60
    : 60
let rootPath = CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : "/Volumes/Adlon/TrueNAS"

print("=== FileID v2 perf harness === root=\(rootPath) seconds=\(Int(runSeconds))")

let root = URL(fileURLWithPath: rootPath)
let bookmark = try root.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil)

let proc = Process()
proc.executableURL = URL(fileURLWithPath: "/Users/adamnolle/Desktop/FileID/.build/debug/FileIDEngine")
let inPipe = Pipe(); let outPipe = Pipe(); let errPipe = Pipe()
proc.standardInput = inPipe; proc.standardOutput = outPipe; proc.standardError = errPipe
try proc.run()
print("Engine pid: \(proc.processIdentifier)")

let dec = JSONDecoder(); dec.dateDecodingStrategy = .iso8601
let enc = JSONEncoder(); enc.dateEncodingStrategy = .iso8601

final class Stats: @unchecked Sendable {
    let lock = NSLock()
    var ready = false
    var discovered = 0
    var totalFiles = 0
    var processed = 0
    var failed = 0
    var filesPerSec: Double = 0
    var residentMB = 0
    var availableMB = 0
    var batches: [BatchSummary] = []
    var phase = "unknown"
    var scanComplete = false
    var firstError: EngineError?
    static func tsNow() -> String {
        let f = ISO8601DateFormatter(); f.formatOptions = [.withInternetDateTime]
        return f.string(from: Date()).suffix(9).description
    }
}
let stats = Stats()

DispatchQueue.global().async {
    var buf = Data()
    while true {
        let chunk = errPipe.fileHandleForReading.availableData
        if chunk.isEmpty { return }
        buf.append(chunk)
        while let nl = buf.firstIndex(of: 0x0A) {
            let line = buf.subdata(in: buf.startIndex..<nl)
            buf.removeSubrange(buf.startIndex...nl)
            if let event = try? dec.decode(IPCEvent.self, from: line) {
                stats.lock.lock()
                switch event.payload {
                case .ready(let info):
                    stats.ready = true
                    print("[\(Stats.tsNow())] READY pid=\(info.pid) workers=\(info.workerCap)")
                case .progress(let p):
                    stats.discovered = p.discovered
                    stats.totalFiles = p.total
                    stats.processed = p.processed
                    stats.failed = p.failed
                    stats.filesPerSec = p.filesPerSecond
                    stats.residentMB = p.residentMB
                    stats.availableMB = p.availableMB
                    stats.phase = p.phase
                case .phaseChanged(let phase):
                    stats.phase = phase
                    print("[\(Stats.tsNow())] PHASE \(phase)")
                case .discoveryComplete(let n):
                    print("[\(Stats.tsNow())] DISCOVERY DONE \(n) files")
                case .batchSummary(let b):
                    stats.batches.append(b)
                    if stats.batches.count % 25 == 0 {
                        print("[\(Stats.tsNow())] BATCH \(b.batchIndex): \(b.filesInBatch) files in \(String(format: "%.2f", b.wallSeconds))s = \(String(format: "%.1f", b.filesPerSecond))/s · RSS \(b.residentMB) MB")
                    }
                case .scanComplete(let c):
                    stats.scanComplete = true
                    print("[\(Stats.tsNow())] SCAN COMPLETE: \(c.processedFiles)/\(c.totalFiles) processed, \(c.failedFiles) failed in \(String(format: "%.1f", c.totalSeconds))s")
                case .error(let e):
                    if stats.firstError == nil { stats.firstError = e }
                    print("[\(Stats.tsNow())] ERROR \(e.kind): \(e.message)")
                case .fileDone, .log: break
                }
                stats.lock.unlock()
            }
        }
    }
}

let readyDeadline = Date().addingTimeInterval(10)
while Date() < readyDeadline {
    stats.lock.lock(); let r = stats.ready; stats.lock.unlock()
    if r { break }
    Thread.sleep(forTimeInterval: 0.1)
}
if !stats.ready { print("FAIL: engine never sent ready"); exit(1) }

let cmd = IPCCommand(id: "harness", payload: .startScan(rootBookmark: bookmark, rootPathDisplay: root.path))
var data = try enc.encode(cmd); data.append(0x0A)
try inPipe.fileHandleForWriting.write(contentsOf: data)
print("[\(Stats.tsNow())] SENT startScan")

let snapshotInterval: Double = 10
let endAt = Date().addingTimeInterval(runSeconds)
var lastSnapshot = Date()
while Date() < endAt {
    Thread.sleep(forTimeInterval: 0.5)
    if Date().timeIntervalSince(lastSnapshot) >= snapshotInterval {
        stats.lock.lock()
        let snap = "[\(Stats.tsNow())] SNAPSHOT phase=\(stats.phase) discovered=\(stats.discovered) processed=\(stats.processed)/\(stats.totalFiles) failed=\(stats.failed) rate=\(String(format: "%.1f", stats.filesPerSec))/s RSS=\(stats.residentMB)MB batches=\(stats.batches.count)"
        let done = stats.scanComplete
        stats.lock.unlock()
        print(snap)
        if done { break }
        lastSnapshot = Date()
    }
}

let cancel = IPCCommand(id: "c", payload: .cancelScan)
var cancelData = try enc.encode(cancel); cancelData.append(0x0A)
try? inPipe.fileHandleForWriting.write(contentsOf: cancelData)
let shut = IPCCommand(id: "s", payload: .shutdown)
var shutData = try enc.encode(shut); shutData.append(0x0A)
try? inPipe.fileHandleForWriting.write(contentsOf: shutData)
try? inPipe.fileHandleForWriting.close()

let exitDeadline = Date().addingTimeInterval(10)
while proc.isRunning && Date() < exitDeadline {
    Thread.sleep(forTimeInterval: 0.2)
}
if proc.isRunning { proc.terminate(); proc.waitUntilExit() }

print("\n=== Final report ===")
stats.lock.lock()
print("Phase at end:   \(stats.phase)")
print("Processed:      \(stats.processed) / \(stats.totalFiles) (discovered=\(stats.discovered), failed=\(stats.failed))")
print("Rolling rate:   \(String(format: "%.1f", stats.filesPerSec)) files/s")
print("Resident:       \(stats.residentMB) MB / avail \(stats.availableMB) MB")
print("Batches:        \(stats.batches.count)")
if !stats.batches.isEmpty {
    let lastN = min(20, stats.batches.count)
    let recent = stats.batches.suffix(lastN)
    let avgRate = recent.map(\.filesPerSecond).reduce(0, +) / Double(lastN)
    let avgInsert = recent.map(\.storeInsertP95Ms).reduce(0, +) / Double(lastN)
    let avgUtil = recent.map(\.utilization).reduce(0, +) / Double(lastN)
    print("Last \(lastN) batches: avg \(String(format: "%.1f", avgRate))/s, avg insertP95=\(String(format: "%.1f", avgInsert))ms, avg util=\(String(format: "%.0f%%", avgUtil*100))")
}
if let e = stats.firstError { print("First error:    \(e.kind) — \(e.message)") }
stats.lock.unlock()
