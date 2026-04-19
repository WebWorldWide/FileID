import SwiftUI
import Vision
import AppKit
import Observation

@MainActor
class AppViewModel: ObservableObject {
    @Published var isProcessing = false
    @Published var processedCount = 0
    @Published var totalCount = 0
    @Published var logs: [String] = []
    
    // Status can be idle, gathering, processing, naming, complete
    @Published var currentStatus: String = "Ready"
    @Published var activeTab: String = "Library"
    @Published var previewFile: FileStatus? = nil
    
    // Using a simple array of file statuses for the UI
    @Published var activeFiles: [FileStatus] = []
    
    // Global Search
    @Published var searchText: String = ""
    
    // Performance Scalability (0 = Low Power, 1 = Balanced, 2 = Max Turbo)
    @Published var performanceProfile: Int = 1
    
    struct FileTreeNode: Identifiable {
        let id: String
        let name: String
        var children: [FileTreeNode]?
        var done: Int = 0
        var total: Int = 0
    }
    @Published var fileTree: [FileTreeNode] = []
    private var treeUpdateTask: Task<Void, Never>?
    
    struct FolderProposal: Identifiable {
        let id = UUID()
        let fileStatus: FileStatus
        let proposedPath: String
    }
    @Published var folderProposals: [FolderProposal] = []
    
    // State Tracking for Timer
    @Published var processingStartTime: Date?
    
    // Global Final Review Toggles
    @Published var applyFilenameRename: Bool = true
    @Published var applyEXIFWrite: Bool = true
    
    var currentFolderURL: URL?
    
    @Observable
    final class FileStatus: Identifiable {
        let id = UUID()
        var filename: String
        var url: URL
        var status: StatusType
        var aiTags: [String] = []
        
        var proposedFilename: String?
        var isSelectedForRename: Bool = true
        var duplicateGroupUUID: UUID? // Tag grouping duplicates together physically
        var scenePrint: VNFeaturePrintObservation? // For semantic duplicate detection
        var thumbnailURL: URL? // Live rendered hardware thumbnails moved to SSD to save RAM
        var isTrashed: Bool = false // Tracks physical disk deletion
        var cameraModel: String? = nil
        var locationString: String? = nil
        var hasFaces: Bool = false
        var fileSizeMB: Double = 0.0
        var creationDateStr: String = ""
        
        var isJunk: Bool {
            if hasFaces { return false } // Safe-guard: Never delete photos of people
            return aiTags.contains("Screenshot") || aiTags.contains("Tax_Document") || aiTags.contains("Text") || aiTags.contains("Receipt") || aiTags.contains("Invoice")
        }
        
        enum StatusType {
            case pending, processing, namingRequired, reviewRequired, completed, failed
        }
        
        init(filename: String, url: URL, status: StatusType) {
            self.filename = filename
            self.url = url
            self.status = status
            
            if let attrs = try? FileManager.default.attributesOfItem(atPath: url.path) {
                let size = attrs[.size] as? Double ?? 0.0
                self.fileSizeMB = size / (1024 * 1024)
                
                if let date = attrs[.creationDate] as? Date {
                    let formatter = DateFormatter()
                    formatter.dateStyle = .short
                    self.creationDateStr = formatter.string(from: date)
                }
            }
        }
    }
    
    func selectFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "Select a folder of photos and videos to scan"
        
        if panel.runModal() == .OK, let url = panel.url {
            let securedAccess = url.startAccessingSecurityScopedResource()
            if !securedAccess {
                log("Warning: system denied security scoping for this URL, will try anyway.")
            }
            startProcessing(folderURL: url)
        }
    }
    
    func runQALoadTest() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.image]
        panel.message = "QA TEST: Select a single heavy photo to clone 10,000 times"
        
        if panel.runModal() == .OK, let url = panel.url {
            Task {
                await MainActor.run { currentStatus = "Generating 10,000 QA Clones..." }
                let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent("QA_Stress_Test_\(UUID().uuidString)")
                try? FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
                
                for i in 1...10000 {
                    let dest = tempDir.appendingPathComponent("QA_Clone_\(i).\(url.pathExtension)")
                    try? FileManager.default.copyItem(at: url, to: dest)
                }
                
                await MainActor.run { currentStatus = "QA Sandbox Ready." }
                startProcessing(folderURL: tempDir)
            }
        }
    }
    
    func startProcessing(folderURL: URL) {
        log("Selected folder: \(folderURL.path)")
        isProcessing = true
        currentFolderURL = folderURL
        currentStatus = "Scanning for media files..."
        
        // Clear previous state
        Task {
            await MainActor.run {
                logs = []
                totalCount = 0
                processedCount = 0
                activeFiles = []
                fileTree = []
                processingStartTime = Date()
            }
            startTreeUpdateLoop()
        }
        
        Task {
            let processor = MediaProcessor(viewModel: self)
            await processor.startDirectoryScan(url: folderURL)
        }
    }
    
    func finishNamingPhase() async {
        guard currentFolderURL != nil else { return }
        await MainActor.run {
            currentStatus = "Preparing Review..."
            processingStartTime = nil // End processing timer
        }
        let processor = MediaProcessor(viewModel: self)
        await processor.preparePreviewNames()
        
        await MainActor.run {
            currentStatus = "Review Changes"
        }
    }
    
    func executeRenaming() async {
        await MainActor.run { currentStatus = "Applying Changes..." }
        await MediaProcessor(viewModel: self).applyIdentityNames(folderURL: currentFolderURL!)
        
        await generateFolderProposals()
    }
    
    private func generateFolderProposals() async {
        guard let root = currentFolderURL else { return }
        let snapshot = await MainActor.run { activeFiles }
        var proposals: [FolderProposal] = []
        
        for file in snapshot {
            if file.status != .reviewRequired || !file.isSelectedForRename { continue }
            
            let date = (try? file.url.resourceValues(forKeys: [.creationDateKey]).creationDate) ?? Date()
            let year = Calendar.current.component(.year, from: date)
            let month = String(format: "%02d", Calendar.current.component(.month, from: date))
            
            var category = "Media"
            let ext = file.url.pathExtension.lowercased()
            if ext == "pdf" {
                if file.aiTags.contains("Invoice") { category = "Documents/Invoices" }
                else if file.aiTags.contains("Receipt") { category = "Documents/Receipts" }
                else if file.aiTags.contains("Tax_Document") { category = "Documents/Taxes" }
                else { category = "Documents" }
            } else {
                if file.aiTags.contains("Screenshot") { category = "Media/Screenshots" }
                else if ext == "mp4" || ext == "mov" { category = "Media/Videos" }
                else { category = "Media/Photos" }
            }
            
            let proposedRelative = "\(year)/\(month)/\(category)/\(file.url.lastPathComponent)"
            
            let currentRelative = file.url.path.replacingOccurrences(of: root.path + "/", with: "")
            if proposedRelative != currentRelative {
                proposals.append(FolderProposal(fileStatus: file, proposedPath: proposedRelative))
            }
        }
        
        await MainActor.run {
            if proposals.isEmpty {
                self.currentStatus = "Complete"
            } else {
                self.folderProposals = proposals
                self.currentStatus = "Restructure Folders"
            }
        }
    }
    
    func applyFolderStructure() async {
        guard let root = currentFolderURL else { return }
        await MainActor.run { currentStatus = "Applying Structure..." }
        
        let snapshot = await MainActor.run { folderProposals }
        
        for proposal in snapshot {
            let currentURL = proposal.fileStatus.url
            let targetURL = root.appendingPathComponent(proposal.proposedPath)
            
            try? FileManager.default.createDirectory(at: targetURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            
            if currentURL != targetURL {
                do {
                    try FileManager.default.moveItem(at: currentURL, to: targetURL)
                    await MainActor.run { proposal.fileStatus.url = targetURL }
                } catch {
                    await log("Failed to move \(currentURL.lastPathComponent) to \(targetURL.path): \(error)")
                }
            }
        }
        
        await MainActor.run { currentStatus = "Complete" }
    }
    
    func approveChanges() async {
        guard let url = currentFolderURL else { return }
        await MainActor.run {
            currentStatus = "Complete"
            isProcessing = false
            url.stopAccessingSecurityScopedResource()
        }
    }
    
    func log(_ message: String) {
        logs.append("[\(Date().formatted(date: .omitted, time: .standard))] \(message)")
        if logs.count > 100 {
            logs.removeFirst(logs.count - 100)
        }
    }
    
    var timeRemainingString: String {
        guard let start = processingStartTime, processedCount > 0, isProcessing else { return "Calculating..." }
        
        let elapsed = Date().timeIntervalSince(start)
        let rate = elapsed / Double(processedCount)
        let remainingFiles = totalCount - processedCount
        
        if remainingFiles == 0 { return "Almost done..." }
        
        let remainingSeconds = rate * Double(remainingFiles)
        return formatTime(seconds: remainingSeconds)
    }
    
    // Helper to format D/H/M/S
    private func formatTime(seconds: Double) -> String {
        // QA Performance Metrics
        if let start = processingStartTime {
            let elapsed = Date().timeIntervalSince(start)
            if elapsed > 5 && processedCount > 0 {
                let filesPerSec = Double(processedCount) / elapsed
                DispatchQueue.main.async {
                    self.log(String(format: "QA METRIC: %.1f files/sec | %.2f GB processed", filesPerSec, (Double(self.processedCount) * 5.0) / 1024.0))
                }
            }
        }
        
        let sec = Int(seconds)
        let d = sec / 86400
        let h = (sec % 86400) / 3600
        let m = (sec % 3600) / 60
        let s = sec % 60
        
        var parts: [String] = []
        if d > 0 { parts.append("\(d)d") }
        if h > 0 { parts.append("\(h)h") }
        if m > 0 { parts.append("\(m)m") }
        if s > 0 || parts.isEmpty { parts.append("\(s)s") }
        
        return parts.joined(separator: " ") + " left"
    }
    
    func startTreeUpdateLoop() {
        treeUpdateTask?.cancel()
        treeUpdateTask = Task {
            while !Task.isCancelled {
                await updateTreeBackground()
                try? await Task.sleep(nanoseconds: 2_000_000_000)
            }
        }
    }
    
    private func updateTreeBackground() async {
        guard let root = currentFolderURL else { return }
        
        class Node {
            var name: String
            var path: String
            var children: [String: Node] = [:]
            var done: Int = 0
            var total: Int = 0
            init(name: String, path: String) { self.name = name; self.path = path }
        }
        
        let rootNode = Node(name: root.lastPathComponent, path: "root")
        let snapshot = await MainActor.run { activeFiles }
        
        var newProposals: [FolderProposal] = []
        
        for file in snapshot {
            let relative = file.url.deletingLastPathComponent().path.replacingOccurrences(of: root.path, with: "")
            let parts = relative.split(separator: "/").map(String.init)
            
            var current = rootNode
            current.total += 1
            let isDone = file.status != .pending && file.status != .processing
            if isDone { current.done += 1 }
            
            var currentPath = "root"
            for part in parts {
                currentPath += "/\(part)"
                if current.children[part] == nil {
                    current.children[part] = Node(name: part, path: currentPath)
                }
                current = current.children[part]!
                current.total += 1
                if isDone { current.done += 1 }
            }
            
            // Generate Live Proposals for completed or analyzed files
            if file.status != .pending && file.status != .processing {
                let date = (try? file.url.resourceValues(forKeys: [.creationDateKey]).creationDate) ?? Date()
                let year = Calendar.current.component(.year, from: date)
                let month = String(format: "%02d", Calendar.current.component(.month, from: date))
                
                var category = "Media"
                let ext = file.url.pathExtension.lowercased()
                if ext == "pdf" {
                    if file.aiTags.contains("Invoice") { category = "Documents/Invoices" }
                    else if file.aiTags.contains("Receipt") { category = "Documents/Receipts" }
                    else if file.aiTags.contains("Tax_Document") { category = "Documents/Taxes" }
                    else { category = "Documents" }
                } else {
                    if file.aiTags.contains("Screenshot") { category = "Media/Screenshots" }
                    else if ext == "mp4" || ext == "mov" { category = "Media/Videos" }
                    else { category = "Media/Photos" }
                }
                
                let proposedRelative = "\(year)/\(month)/\(category)/\(file.url.lastPathComponent)"
                let currentRelative = file.url.path.replacingOccurrences(of: root.path + "/", with: "")
                
                if proposedRelative != currentRelative {
                    newProposals.append(FolderProposal(fileStatus: file, proposedPath: proposedRelative))
                }
            }
        }
        
        func convert(node: Node) -> FileTreeNode {
            let kids = node.children.isEmpty ? nil : node.children.values.map { convert(node: $0) }.sorted { $0.name < $1.name }
            return FileTreeNode(id: node.path, name: node.name, children: kids, done: node.done, total: node.total)
        }
        
        let newTree = [convert(node: rootNode)]
        await MainActor.run {
            self.fileTree = newTree
            self.folderProposals = newProposals
        }
    }
}
