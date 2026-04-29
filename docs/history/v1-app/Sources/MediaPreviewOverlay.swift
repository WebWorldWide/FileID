import SwiftUI
import SwiftData
import QuickLookUI
import AVKit
import ImageIO
import CoreLocation

struct MediaPreviewOverlay: View {
    @ObservedObject var viewModel: AppViewModel
    @State private var showEXIF = false
    @State private var exifData: [String: String] = [:]
    @State private var showNameToggle = false   // false = current, true = proposed
    @State private var deepAnalyzing = false
    @State private var deepResult: String?
    @Environment(\.modelContext) private var modelContext

    // Navigation always uses the caller-supplied list via viewModel.openPreview(_:in:).
    // Previously we had an unbounded @Query fallback that materialized every
    // FileRecord (50 K+) on every overlay open — that made the overlay take
    // several seconds to appear on large libraries.
    private var navigationFiles: [FileRecord] { viewModel.previewList }

    var currentIndex: Int? {
        guard let file = viewModel.previewFile else { return nil }
        if let byID = navigationFiles.firstIndex(where: { $0.id == file.id }) {
            return byID
        }
        let path = file.url.path
        if let byPath = navigationFiles.firstIndex(where: { $0.url.path == path }) {
            return byPath
        }
        // The current preview file isn't in the navigation list — likely
        // deleted or filtered out since the overlay opened. Falling back
        // to 0 (rather than nil) keeps the nav buttons live so the user
        // can scroll to a still-valid file instead of being stuck.
        return navigationFiles.isEmpty ? nil : 0
    }

    var body: some View {
        if let file = viewModel.previewFile {
            ZStack {
                Color.black.opacity(0.88)
                    .ignoresSafeArea()
                    .onTapGesture {
                        withAnimation(.easeOut(duration: 0.2)) { viewModel.closePreview() }
                    }

                HStack(spacing: 0) {
                    VStack(spacing: 0) {
                        HStack(spacing: 12) {
                            VStack(alignment: .leading, spacing: 2) {
                                if showNameToggle, let proposed = file.proposedFilename {
                                    HStack(spacing: 6) {
                                        Image(systemName: "wand.and.stars")
                                            .font(.caption)
                                            .foregroundStyle(Theme.gold)
                                        Text(proposed)
                                            .font(.system(size: 13, weight: .semibold))
                                            .foregroundStyle(Theme.gold)
                                            .lineLimit(1)
                                    }
                                } else {
                                    Text(file.filename)
                                        .font(.system(size: 13, weight: .semibold))
                                        .foregroundStyle(.white)
                                        .lineLimit(1)
                                }
                                Text(file.url.deletingLastPathComponent().path)
                                    .font(.system(size: 10))
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                                    .truncationMode(.head)
                            }
                            .onTapGesture {
                                guard file.proposedFilename != nil else { return }
                                withAnimation(.spring(response: 0.3)) { showNameToggle.toggle() }
                            }
                            .help(file.proposedFilename != nil ? "Tap to toggle proposed filename" : "")

                            Spacer()

                            ScrollView(.horizontal, showsIndicators: false) {
                                HStack(spacing: 4) {
                                    ForEach(file.aiTags, id: \.self) { tag in
                                        Text(tag)
                                            .font(.system(size: 9, weight: .semibold))
                                            .lineLimit(1)
                                            .fixedSize()
                                            .padding(.horizontal, 6)
                                            .padding(.vertical, 3)
                                            .background(Capsule().fill(Theme.gold.opacity(0.2)))
                                            .foregroundStyle(Theme.gold)
                                    }
                                }
                            }
                            .frame(maxWidth: 300, maxHeight: 22)

                            Button {
                                withAnimation(.spring(response: 0.4)) {
                                    showEXIF.toggle()
                                    if showEXIF { loadEXIF(from: file.url) }
                                }
                            } label: {
                                Label("Info", systemImage: showEXIF ? "info.circle.fill" : "info.circle")
                                    .font(.system(size: 12))
                            }
                            .buttonStyle(.bordered)
                            .tint(showEXIF ? Theme.gold : nil)
                            .help(showEXIF ? "Hide EXIF metadata panel" : "Show EXIF metadata panel")

                            let deepModelReady = DeepAnalyzeService.isInstalledOnDisk()
                            let deepBlocked = viewModel.isProcessing
                            Button {
                                if deepModelReady {
                                    runDeepAnalyze(file: file)
                                } else {
                                    showDeepModelMissingAlert()
                                }
                            } label: {
                                if deepAnalyzing {
                                    ProgressView().controlSize(.small)
                                } else {
                                    Label("Deep Analyze", systemImage: "sparkles.rectangle.stack")
                                        .font(.system(size: 12))
                                }
                            }
                            .buttonStyle(.bordered)
                            .tint(.purple)
                            .disabled(deepAnalyzing || deepBlocked)
                            .help(deepBlocked
                                  ? "Pause or finish scanning to run Deep Analyze — Qwen2.5-VL needs ~3 GB of VRAM that scan is currently holding"
                                  : (deepModelReady
                                     ? "Run a local VLM (Qwen2.5-VL 3B) for a rich caption and category"
                                     : "Qwen2.5-VL 3B isn't installed yet — click to set it up in Settings → AI Models"))

                            Button {
                                NSWorkspace.shared.activateFileViewerSelecting([file.url])
                            } label: {
                                Label("Show in Finder", systemImage: "folder")
                                    .font(.system(size: 12))
                            }
                            .buttonStyle(.bordered)
                            .help("Reveal this file in Finder")

                            Button {
                                withAnimation(.easeOut(duration: 0.2)) { viewModel.closePreview() }
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .font(.system(size: 28))
                                    .symbolRenderingMode(.palette)
                                    .foregroundStyle(.white.opacity(0.7), .white.opacity(0.15))
                            }
                            .buttonStyle(.plain)
                            .contentShape(Rectangle())
                            .help("Close preview")
                        }
                        .padding(.horizontal, 20)
                        .padding(.vertical, 12)
                        .background(.ultraThinMaterial)

                        ZStack {
                            let ext = file.url.pathExtension.lowercased()
                            if ["mp4", "mov"].contains(ext) {
                                VideoPlayerView(url: file.url)
                                    .padding(20)
                            } else if ext == "pdf" {
                                QuickLookPreviewView(url: file.url)
                                    .padding(20)
                            } else {
                                AsyncImageView(url: file.url)
                                    .padding(20)
                            }
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)

                        if let deepResult, !deepResult.isEmpty {
                            VStack(alignment: .leading, spacing: 6) {
                                HStack(spacing: 6) {
                                    Image(systemName: "sparkles.rectangle.stack.fill")
                                        .foregroundStyle(.purple)
                                    Text("Deep Analysis (Qwen2-VL)")
                                        .font(.caption.bold())
                                        .foregroundStyle(.purple)
                                    Spacer()
                                    Button {
                                        self.deepResult = nil
                                    } label: {
                                        Image(systemName: "xmark.circle.fill")
                                            .font(.system(size: 14))
                                            .foregroundStyle(.secondary)
                                    }
                                    .buttonStyle(.plain)
                                    .contentShape(Rectangle())
                                    .help("Dismiss deep analysis result")
                                }
                                ScrollView {
                                    Text(deepResult)
                                        .font(.system(size: 12))
                                        .foregroundStyle(.primary)
                                        .textSelection(.enabled)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                }
                                .frame(maxHeight: 140)
                            }
                            .padding(12)
                            .background(
                                RoundedRectangle(cornerRadius: 10)
                                    .fill(.ultraThinMaterial)
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 10)
                                            .stroke(.purple.opacity(0.35), lineWidth: 1)
                                    )
                            )
                            .padding(.horizontal, 20)
                            .padding(.bottom, 8)
                            .transition(.move(edge: .bottom).combined(with: .opacity))
                        }

                        HStack(spacing: 20) {
                            Button {
                                navigateBy(-1)
                            } label: {
                                Image(systemName: "arrow.left.circle.fill")
                                    .font(.system(size: 28))
                                    .foregroundStyle(Theme.gold)
                            }
                            .buttonStyle(.plain)
                            .contentShape(Rectangle())
                            .disabled(currentIndex == nil || currentIndex == 0)
                            .opacity((currentIndex ?? 0) > 0 ? 1.0 : 0.3)
                            .keyboardShortcut(.leftArrow, modifiers: [])
                            .help("Previous file (←)")

                            Spacer()

                            HStack(spacing: 16) {
                                if let cam = file.cameraModel {
                                    Label(cam, systemImage: "camera.fill")
                                        .font(.system(size: 10))
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                        .truncationMode(.tail)
                                }
                                if let loc = file.locationString {
                                    Label(loc, systemImage: "location.fill")
                                        .font(.system(size: 10))
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                        .truncationMode(.tail)
                                }
                                Text(file.creationDate.formatted(date: .long, time: .shortened))
                                    .font(.system(size: 10))
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                                    .truncationMode(.tail)
                                Text("·")
                                    .foregroundStyle(.tertiary)
                                Text(String(format: "%.1f MB", file.fileSizeMB))
                                    .font(.system(size: 10, design: .monospaced))
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                            }
                            .layoutPriority(1)

                            Spacer()

                            Button {
                                navigateBy(1)
                            } label: {
                                Image(systemName: "arrow.right.circle.fill")
                                    .font(.system(size: 28))
                                    .foregroundStyle(Theme.gold)
                            }
                            .buttonStyle(.plain)
                            .contentShape(Rectangle())
                            .disabled(currentIndex == nil || currentIndex == navigationFiles.count - 1)
                            .opacity((currentIndex ?? 0) < navigationFiles.count - 1 ? 1.0 : 0.3)
                            .keyboardShortcut(.rightArrow, modifiers: [])
                            .help("Next file (→)")
                        }
                        .padding(.horizontal, 24)
                        .padding(.vertical, 12)
                        .background(.ultraThinMaterial)
                    }

                    if showEXIF {
                        EXIFPanel(data: exifData, file: file)
                            .frame(width: 300)
                            .transition(.move(edge: .trailing).combined(with: .opacity))
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .clipShape(RoundedRectangle(cornerRadius: 20))
                .shadow(color: .black.opacity(0.5), radius: 40, y: 20)
                .padding(40)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .ignoresSafeArea()
            .onKeyPress(.escape) {
                withAnimation { viewModel.closePreview() }
                return .handled
            }
            .task {
                deepResult = file.deepAnalysis
            }
            .onChange(of: viewModel.previewFile?.id) { _, _ in
                deepResult = viewModel.previewFile?.deepAnalysis
                deepAnalyzing = false
            }
        }
    }

    // MARK: - Helpers

    func navigateBy(_ delta: Int) {
        let files = navigationFiles
        guard let current = viewModel.previewFile,
              let idx = files.firstIndex(where: { $0.id == current.id }) else { return }
        var newIdx = idx + delta
        var steps = 0
        // Skip records that aren't ready for preview yet.
        while files.indices.contains(newIdx) && steps < files.count {
            let c = files[newIdx]
            if c.status != .pending && c.status != .processing { break }
            newIdx += delta
            steps += 1
        }
        guard files.indices.contains(newIdx) else { return }
        withAnimation(.easeInOut(duration: 0.15)) {
            viewModel.previewFile = files[newIdx]
            showEXIF = false
            exifData = [:]
        }
    }

    private func showDeepModelMissingAlert() {
        let alert = NSAlert()
        alert.messageText = "Qwen2-VL 2B isn't installed"
        alert.informativeText = "Deep Analyze needs a ~1.5 GB vision-language model. Download it once in Settings → AI Models; subsequent runs are instant."
        alert.alertStyle = .informational
        alert.addButton(withTitle: "Open Settings")
        alert.addButton(withTitle: "Cancel")
        if alert.runModal() == .alertFirstButtonReturn {
            NotificationCenter.default.post(name: .fileIDOpenAIModelSettings, object: nil)
        }
    }

    private func runDeepAnalyze(file: FileRecord) {
        guard !deepAnalyzing else { return }
        deepAnalyzing = true
        deepResult = nil
        Task {
            let url = file.url
            let text = await DeepAnalyzeService.shared.analyze(imageURL: url)
            await MainActor.run {
                deepResult = text
                file.deepAnalysis = text
                try? modelContext.save()
                deepAnalyzing = false
            }
        }
    }

    func loadEXIF(from url: URL) {
        Task.detached(priority: .userInitiated) {
            guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
                  let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] else { return }

            var result: [String: String] = [:]

            if let exif = props[kCGImagePropertyExifDictionary] as? [CFString: Any] {
                if let iso = exif[kCGImagePropertyExifISOSpeedRatings] as? [Int] { result["ISO"] = "\(iso.first ?? 0)" }
                if let exp = exif[kCGImagePropertyExifExposureTime] as? Double { result["Exposure"] = String(format: "1/%.0f s", 1/exp) }
                if let ap = exif[kCGImagePropertyExifFNumber] as? Double { result["Aperture"] = String(format: "f/%.1f", ap) }
                if let fl = exif[kCGImagePropertyExifFocalLength] as? Double { result["Focal Length"] = "\(Int(fl))mm" }
                if let dt = exif[kCGImagePropertyExifDateTimeOriginal] as? String { result["Captured"] = dt }
                if let lens = exif[kCGImagePropertyExifLensModel] as? String { result["Lens"] = lens }
            }
            if let tiff = props[kCGImagePropertyTIFFDictionary] as? [CFString: Any] {
                if let make = tiff[kCGImagePropertyTIFFMake] as? String { result["Make"] = make }
                if let model = tiff[kCGImagePropertyTIFFModel] as? String { result["Camera"] = model }
                if let sw = tiff[kCGImagePropertyTIFFSoftware] as? String { result["Software"] = sw }
            }
            if let gps = props[kCGImagePropertyGPSDictionary] as? [CFString: Any] {
                if let lat = gps[kCGImagePropertyGPSLatitude] as? Double,
                   let lon = gps[kCGImagePropertyGPSLongitude] as? Double {
                    let latRef = gps[kCGImagePropertyGPSLatitudeRef] as? String ?? "N"
                    let lonRef = gps[kCGImagePropertyGPSLongitudeRef] as? String ?? "W"
                    result["GPS"] = String(format: "%.5f°%@ %.5f°%@", lat, latRef, lon, lonRef)
                    if let alt = gps[kCGImagePropertyGPSAltitude] {
                        let meters = (alt as? Double) ?? (alt as? NSNumber)?.doubleValue ?? 0
                        result["Altitude"] = String(format: "%.0f m", meters)
                    }
                    let finalLat = latRef == "S" ? -lat : lat
                    let finalLon = lonRef == "W" ? -lon : lon
                    let location = CLLocation(latitude: finalLat, longitude: finalLon)
                    if let places = try? await CLGeocoder().reverseGeocodeLocation(location),
                       let place  = places.first {
                        var parts: [String] = []
                        if let city  = place.locality           { parts.append(city) }
                        if let state = place.administrativeArea { parts.append(state) }
                        if !parts.isEmpty { result["Location"] = parts.joined(separator: ", ") }
                    }
                }
            }
            if let px = props[kCGImagePropertyPixelWidth] as? Int,
               let py = props[kCGImagePropertyPixelHeight] as? Int {
                result["Resolution"] = "\(px) × \(py) px"
            }
            if let dpi = props[kCGImagePropertyDPIWidth] as? Int { result["DPI"] = "\(dpi)" }

            let finalResult = result
            await MainActor.run { exifData = finalResult }
        }
    }
}

// MARK: - EXIF Panel

struct EXIFPanel: View {
    let data: [String: String]
    let file: FileRecord

    let fieldOrder = ["Camera", "Make", "Lens", "Focal Length", "Aperture",
                      "Exposure", "ISO", "Resolution", "DPI", "Captured",
                      "GPS", "Altitude", "Location", "Software"]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Image(systemName: "info.circle.fill")
                    .foregroundStyle(Theme.gold)
                Text("File Info")
                    .font(.system(size: 13, weight: .bold))
                Spacer()
            }
            .padding(14)
            .background(Color(white: 0.12))

            if data.isEmpty {
                Spacer()
                HStack {
                    Spacer()
                    VStack(spacing: 8) {
                        ProgressView()
                        Text("Loading metadata…")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                }
                Spacer()
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        let ordered = fieldOrder.compactMap { key -> (String, String)? in
                            guard let val = data[key] else { return nil }
                            return (key, val)
                        }
                        let extra = data.filter { !fieldOrder.contains($0.key) }
                            .sorted { $0.key < $1.key }

                        ForEach(Array((ordered + extra.map { ($0.key, $0.value) }).enumerated()), id: \.offset) { _, pair in
                            EXIFRow(key: pair.0, value: pair.1)
                        }
                    }
                    .padding(.vertical, 8)
                }
            }

            Divider()

            VStack(alignment: .leading, spacing: 4) {
                Text("Path")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(.tertiary)
                Text(file.url.path)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(4)
                    .textSelection(.enabled)

                Button("Reveal in Finder") {
                    NSWorkspace.shared.activateFileViewerSelecting([file.url])
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .padding(.top, 4)
                .help("Show this file in Finder")
            }
            .padding(12)
            .background(Color(white: 0.1))
        }
        .background(Color(white: 0.09))
        .clipShape(RoundedRectangle(cornerRadius: 0))
    }
}

struct EXIFRow: View {
    let key: String
    let value: String

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Text(key)
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(.secondary)
                .frame(width: 90, alignment: .trailing)
            Text(value)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.white.opacity(0.85))
                .textSelection(.enabled)
            Spacer()
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 5)
        .background(Color.white.opacity(0.025))
    }
}

// MARK: - Video Player

// SwiftUI's VideoPlayer crashed on macOS 26 inside _swift_instantiateGenericMetadata;
// AVPlayerView wrapped as NSViewRepresentable sidesteps the generic-metadata path.

struct VideoPlayerView: View {
    let url: URL
    @State private var player: AVPlayer?

    var body: some View {
        Group {
            if let player {
                AVPlayerViewRepresentable(player: player)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
            } else {
                ProgressView()
                    .tint(Theme.gold)
            }
        }
        .task(id: url) {
            player?.pause()
            player = nil                // release old decoder before allocating a new one
            player = AVPlayer(url: url)
        }
        .onDisappear {
            player?.pause()
            player = nil
        }
    }
}

private struct AVPlayerViewRepresentable: NSViewRepresentable {
    let player: AVPlayer

    func makeNSView(context: Context) -> AVPlayerView {
        let view = AVPlayerView()
        view.player = player
        view.controlsStyle = .inline
        view.videoGravity = .resizeAspect
        view.showsFullScreenToggleButton = true
        return view
    }

    func updateNSView(_ nsView: AVPlayerView, context: Context) {
        if nsView.player !== player {
            nsView.player = player
        }
    }
}

// MARK: - QuickLookPreviewView

struct QuickLookPreviewView: NSViewRepresentable {
    let url: URL

    func makeNSView(context: Context) -> QLPreviewView {
        let view = QLPreviewView(frame: .zero, style: .normal) ?? QLPreviewView()
        view.previewItem = url as QLPreviewItem
        return view
    }

    func updateNSView(_ nsView: QLPreviewView, context: Context) {
        nsView.previewItem = url as QLPreviewItem
    }
}

// MARK: - AsyncImageView

struct AsyncImageView: View {
    let url: URL
    @State private var loadedImage: NSImage?
    @State private var hasError = false

    var body: some View {
        Group {
            if let img = loadedImage {
                Image(nsImage: img)
                    .resizable()
                    .scaledToFit()
                    .clipShape(RoundedRectangle(cornerRadius: 12))
            } else if hasError {
                VStack(spacing: 10) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 36))
                        .foregroundStyle(.red.opacity(0.7))
                    Text("Unable to load preview")
                        .foregroundStyle(.secondary)
                }
            } else {
                ProgressView()
                    .controlSize(.large)
                    .tint(Theme.gold)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task {
            let t = Task.detached(priority: .userInitiated) { () -> NSImage? in
                guard !Task.isCancelled else { return nil }
                if let data = try? Data(contentsOf: url, options: .mappedIfSafe) {
                    return NSImage(data: data)
                }
                return nil
            }
            if let img = await t.value { loadedImage = img } else { hasError = true }
        }
    }
}
