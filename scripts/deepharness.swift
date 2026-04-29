// Deep Analyze test harness. Steps:
//   1. Spawn the engine.
//   2. Run a scan against the user's smaller test folder (default:
//      ~/Pictures or whatever path is passed as arg 2). Wait until
//      `scanComplete` arrives.
//   3. Send `deepAnalyzeAll` for the safe default model (Qwen 2.5-VL 3B).
//      Skip-existing = false so we exercise inference fully.
//   4. Watch deepAnalyzeProgress + deepAnalyzeFileDone events. Print one
//      every 10 completions + every 30 s.
//   5. Stop at N completions or after `seconds` wall time, whichever first.
//   6. Print a final summary including how many files actually got
//      vlm_description / vlm_proposed_name written to the DB.
import Foundation

// MARK: - IPC types (subset)

struct IPCCommand: Codable {
    let id: String; let payload: Payload
    enum Payload: Codable {
        case startScan(rootBookmark: Data, rootPathDisplay: String)
        case pauseScan, resumeScan, cancelScan, requestStatus, shutdown
        case runFaceClustering
        case deepAnalyzeFile(fileID: Int64, modelKind: String)
        case deepAnalyzeFolder(pathPrefix: String, modelKind: String)
        case deepAnalyzeAll(modelKind: String, skipExisting: Bool)
        case deepAnalyzeCancel
    }
}
struct IPCEvent: Codable {
    let t: Date; let payload: Payload
    enum Payload: Codable {
        case ready(EngineInfo)
        case progress(ScanProgress)
        case phaseChanged(String)
        case discoveryComplete(totalFiles: Int)
        case fileDone(Empty); case batchSummary(Empty); case scanComplete(ScanComplete)
        case error(EngineError); case log(Empty)
        case faceClusteringComplete(Empty)
        case deepAnalyzeProgress(DeepAnalyzeProgress)
        case deepAnalyzeFileDone(DeepAnalyzeFileDone)
        case deepAnalyzeComplete(DeepAnalyzeComplete)
        case modelDownloadProgress(ModelDownloadProgress)
        case queueState(Empty)
    }
}
struct EngineInfo: Codable { let version: String; let pid: Int32; let workerCap: Int; let physicalMemoryGB: Double }
struct ScanProgress: Codable {
    let sessionID: String; let phase: String; let total: Int
    let discovered: Int; let processed: Int; let failed: Int
    let filesPerSecond: Double; let etaSeconds: Double?
    let residentMB: Int; let availableMB: Int
}
struct ScanComplete: Codable { let sessionID: String; let totalFiles: Int; let processedFiles: Int; let failedFiles: Int; let totalSeconds: Double }
struct EngineError: Codable { let kind: String; let message: String; let path: String? }
struct DeepAnalyzeProgress: Codable { let processed: Int; let total: Int; let etaSeconds: Double?; let currentPath: String?; let modelKind: String }
struct DeepAnalyzeFileDone: Codable { let fileID: Int64; let description: String; let proposedName: String?; let modelKind: String }
struct DeepAnalyzeComplete: Codable { let processed: Int; let failed: Int; let totalSeconds: Double; let modelKind: String; let cancelled: Bool }
struct ModelDownloadProgress: Codable { let modelKind: String; let fraction: Double; let message: String }
struct Empty: Codable {}

// MARK: - Args

let runSeconds: Double = CommandLine.arguments.count > 1
    ? Double(CommandLine.arguments[1]) ?? 600
    : 600
let rootPath = CommandLine.arguments.count > 2
    ? CommandLine.arguments[2]
    : NSString(string: "~/Pictures").expandingTildeInPath
let modelKey = CommandLine.arguments.count > 3
    ? CommandLine.arguments[3]
    : "qwen2_vl_3b"

print("=== Deep Analyze harness ===")
print("Root:           \(rootPath)")
print("Model:          \(modelKey)")
print("Wall budget:    \(Int(runSeconds))s")
print("")

let root = URL(fileURLWithPath: rootPath)
let bookmark = try root.bookmarkData(options: [], includingResourceValuesForKeys: nil, relativeTo: nil)

let proc = Process()
proc.executableURL = URL(fileURLWithPath: "/Users/adamnolle/Desktop/FileID/.build/debug/FileIDEngine")
let inPipe = Pipe(); let outPipe = Pipe(); let errPipe = Pipe()
proc.standardInput = inPipe; proc.standardOutput = outPipe; proc.standardError = errPipe
try proc.run()
print("Engine pid:     \(proc.processIdentifier)")
print("")

let dec = JSONDecoder(); dec.dateDecodingStrategy = .iso8601
let enc = JSONEncoder(); enc.dateEncodingStrategy = .iso8601

final class State: @unchecked Sendable {
    let lock = NSLock()
    var ready = false
    var scanComplete: ScanComplete?
    var deepProgress: DeepAnalyzeProgress?
    var deepCompletes: [DeepAnalyzeComplete] = []
    var deepFilesDone: [DeepAnalyzeFileDone] = []
    var modelDownload: ModelDownloadProgress?
    var firstError: EngineError?
}
let state = State()

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
                state.lock.lock()
                switch event.payload {
                case .ready(let info):
                    state.ready = true
                    print("[ready]   pid \(info.pid) workers \(info.workerCap)")
                case .scanComplete(let c):
                    state.scanComplete = c
                    print("[scan]    DONE \(c.processedFiles)/\(c.totalFiles) in \(String(format: "%.1f", c.totalSeconds))s, \(c.failedFiles) failed")
                case .modelDownloadProgress(let p):
                    state.modelDownload = p
                    if Int(p.fraction * 100) % 10 == 0 {
                        print("[model]   \(p.message)")
                    }
                case .deepAnalyzeProgress(let p):
                    state.deepProgress = p
                    if p.processed % 10 == 0 {
                        let etaStr = p.etaSeconds.map { "ETA \(Int($0))s" } ?? ""
                        print("[deep]    \(p.processed)/\(p.total) \(etaStr)  \(p.currentPath?.split(separator: "/").suffix(2).joined(separator: "/") ?? "")")
                    }
                case .deepAnalyzeFileDone(let d):
                    state.deepFilesDone.append(d)
                    if state.deepFilesDone.count <= 5 || state.deepFilesDone.count % 50 == 0 {
                        let nm = d.proposedName ?? "<no name>"
                        print("[file]    fileID=\(d.fileID)  name='\(nm)'")
                        print("          \(String(d.description.prefix(140)))")
                    }
                case .deepAnalyzeComplete(let c):
                    state.deepCompletes.append(c)
                    print("[deep]    DONE \(c.processed) processed, \(c.failed) failed in \(String(format: "%.1f", c.totalSeconds))s, cancelled=\(c.cancelled)")
                case .error(let e):
                    if state.firstError == nil { state.firstError = e }
                    print("[ERROR]   \(e.kind): \(e.message)")
                default:
                    break
                }
                state.lock.unlock()
            }
        }
    }
}

// Wait ready.
let readyDeadline = Date().addingTimeInterval(10)
while Date() < readyDeadline {
    state.lock.lock(); let r = state.ready; state.lock.unlock()
    if r { break }
    Thread.sleep(forTimeInterval: 0.1)
}
guard state.ready else { print("FAIL: engine never sent ready"); exit(1) }

// 1. Send startScan.
let scanCmd = IPCCommand(id: "scan", payload: .startScan(rootBookmark: bookmark, rootPathDisplay: root.path))
var scanData = try enc.encode(scanCmd); scanData.append(0x0A)
try inPipe.fileHandleForWriting.write(contentsOf: scanData)
print("[harness] sent startScan, waiting for scanComplete…")

// Wait for scanComplete or 5min cap.
let scanDeadline = Date().addingTimeInterval(300)
while Date() < scanDeadline {
    state.lock.lock(); let done = state.scanComplete != nil; state.lock.unlock()
    if done { break }
    Thread.sleep(forTimeInterval: 1.0)
}
guard state.scanComplete != nil else {
    print("FAIL: scan didn't complete within 5 min")
    proc.terminate()
    exit(2)
}

// 2. Send deepAnalyzeAll.
let deepCmd = IPCCommand(id: "deep", payload: .deepAnalyzeAll(modelKind: modelKey, skipExisting: false))
var deepData = try enc.encode(deepCmd); deepData.append(0x0A)
try inPipe.fileHandleForWriting.write(contentsOf: deepData)
print("[harness] sent deepAnalyzeAll, watching for ~\(Int(runSeconds))s…")

// Watch.
let endAt = Date().addingTimeInterval(runSeconds)
var lastSnapshot = Date()
while Date() < endAt {
    Thread.sleep(forTimeInterval: 1.0)
    if Date().timeIntervalSince(lastSnapshot) >= 30 {
        state.lock.lock()
        let done = state.deepFilesDone.count
        let progress = state.deepProgress
        state.lock.unlock()
        let ts = ISO8601DateFormatter().string(from: Date()).suffix(8)
        let progStr = progress.map { "\($0.processed)/\($0.total)" } ?? "?"
        print("[\(ts)] heartbeat: \(done) files done so far · progress=\(progStr)")
        lastSnapshot = Date()
    }
    state.lock.lock()
    let allDone = state.deepCompletes.first != nil
    state.lock.unlock()
    if allDone { break }
}

// 3. Cancel + shutdown to wind down cleanly.
let cancel = IPCCommand(id: "c", payload: .deepAnalyzeCancel)
var cancelData = try enc.encode(cancel); cancelData.append(0x0A)
try? inPipe.fileHandleForWriting.write(contentsOf: cancelData)
let shut = IPCCommand(id: "s", payload: .shutdown)
var shutData = try enc.encode(shut); shutData.append(0x0A)
try? inPipe.fileHandleForWriting.write(contentsOf: shutData)
try? inPipe.fileHandleForWriting.close()

let exitDeadline = Date().addingTimeInterval(15)
while proc.isRunning && Date() < exitDeadline {
    Thread.sleep(forTimeInterval: 0.2)
}
if proc.isRunning { proc.terminate(); proc.waitUntilExit() }

print("")
print("=== Final report ===")
state.lock.lock()
print("Scan:           \(state.scanComplete.map { "OK \($0.processedFiles)/\($0.totalFiles)" } ?? "MISSING")")
print("Model download: \(state.modelDownload?.message ?? "(model already on disk)")")
print("Files done:     \(state.deepFilesDone.count)")
if let c = state.deepCompletes.first {
    let perFile = c.processed > 0 ? c.totalSeconds / Double(c.processed) : 0
    print("Final summary:  \(c.processed) ok / \(c.failed) failed in \(String(format: "%.1f", c.totalSeconds))s = \(String(format: "%.2f", perFile))s/image")
}
if !state.deepFilesDone.isEmpty {
    print("")
    print("First 3 captions:")
    for d in state.deepFilesDone.prefix(3) {
        print("  fileID=\(d.fileID)  name='\(d.proposedName ?? "?")'")
        print("    \(d.description.prefix(160))")
    }
}
if let e = state.firstError { print("First error:    \(e.kind) — \(e.message)") }
state.lock.unlock()
