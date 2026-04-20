import SwiftUI
import SwiftData
import QuickLookUI
import AVKit
import ImageIO

struct MediaPreviewOverlay: View {
    @ObservedObject var viewModel: AppViewModel
    @State private var showEXIF = false
    @State private var exifData: [String: String] = [:]
    @State private var showNameToggle = false   // false = current, true = proposed
    @Environment(\.modelContext) private var modelContext

    // All eligible files for arrow-key navigation
    var allFiles: [FileRecord] {
        (try? modelContext.fetch(FetchDescriptor<FileRecord>())) ?? []
    }

    var currentIndex: Int? {
        guard let file = viewModel.previewFile else { return nil }
        return allFiles.firstIndex { $0.id == file.id }
    }

    var body: some View {
        if let file = viewModel.previewFile {
            ZStack {
                // Dimmed background
                Color.black.opacity(0.88)
                    .ignoresSafeArea()
                    .onTapGesture {
                        withAnimation(.easeOut(duration: 0.2)) { viewModel.previewFile = nil }
                    }

                HStack(spacing: 0) {
                    // ── Main preview ─────────────────────────────────
                    VStack(spacing: 0) {
                        // Top bar
                        HStack(spacing: 12) {
                            // Name toggle (original ↔ proposed)
                            VStack(alignment: .leading, spacing: 2) {
                                if showNameToggle, let proposed = file.proposedFilename {
                                    HStack(spacing: 6) {
                                        Image(systemName: "wand.and.stars")
                                            .font(.caption)
                                            .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                                        Text(proposed)
                                            .font(.system(size: 13, weight: .semibold))
                                            .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
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

                            // Tags inline
                            ScrollView(.horizontal, showsIndicators: false) {
                                HStack(spacing: 4) {
                                    ForEach(file.aiTags, id: \.self) { tag in
                                        Text(tag)
                                            .font(.system(size: 9, weight: .semibold))
                                            .lineLimit(1)
                                            .fixedSize()
                                            .padding(.horizontal, 6)
                                            .padding(.vertical, 3)
                                            .background(Capsule().fill(Color(red: 1.0, green: 0.8, blue: 0.0).opacity(0.2)))
                                            .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                                    }
                                }
                            }
                            .frame(maxWidth: 300, maxHeight: 22)

                            // EXIF toggle
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
                            .tint(showEXIF ? Color(red: 1.0, green: 0.8, blue: 0.0) : nil)

                            // Close
                            Button {
                                withAnimation(.easeOut(duration: 0.2)) { viewModel.previewFile = nil }
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .font(.system(size: 28))
                                    .symbolRenderingMode(.palette)
                                    .foregroundStyle(.white.opacity(0.7), .white.opacity(0.15))
                            }
                            .buttonStyle(.plain)
                        }
                        .padding(.horizontal, 20)
                        .padding(.vertical, 12)
                        .background(.ultraThinMaterial)

                        // Content
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

                        // Bottom: nav arrows + metadata
                        HStack(spacing: 20) {
                            // Arrow navigation
                            Button {
                                navigateBy(-1)
                            } label: {
                                Image(systemName: "arrow.left.circle.fill")
                                    .font(.system(size: 28))
                                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                            }
                            .buttonStyle(.plain)
                            .disabled(currentIndex == nil || currentIndex == 0)
                            .opacity((currentIndex ?? 0) > 0 ? 1 : 0.3)
                            .keyboardShortcut(.leftArrow, modifiers: [])

                            Spacer()

                            // File metadata
                            HStack(spacing: 16) {
                                if let cam = file.cameraModel {
                                    Label(cam, systemImage: "camera.fill")
                                        .font(.system(size: 10))
                                        .foregroundStyle(.secondary)
                                }
                                if let loc = file.locationString {
                                    Label(loc, systemImage: "location.fill")
                                        .font(.system(size: 10))
                                        .foregroundStyle(.secondary)
                                }
                                Text(file.creationDate.formatted(date: .long, time: .shortened))
                                    .font(.system(size: 10))
                                    .foregroundStyle(.secondary)
                                Text("·")
                                    .foregroundStyle(.tertiary)
                                Text(String(format: "%.1f MB", file.fileSizeMB))
                                    .font(.system(size: 10, design: .monospaced))
                                    .foregroundStyle(.secondary)
                            }

                            Spacer()

                            Button {
                                navigateBy(1)
                            } label: {
                                Image(systemName: "arrow.right.circle.fill")
                                    .font(.system(size: 28))
                                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
                            }
                            .buttonStyle(.plain)
                            .disabled(currentIndex == nil || currentIndex == allFiles.count - 1)
                            .opacity((currentIndex ?? 0) < allFiles.count - 1 ? 1 : 0.3)
                            .keyboardShortcut(.rightArrow, modifiers: [])
                        }
                        .padding(.horizontal, 24)
                        .padding(.vertical, 12)
                        .background(.ultraThinMaterial)
                    }

                    // ── EXIF Inspector panel ──────────────────────────
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
                withAnimation { viewModel.previewFile = nil }
                return .handled
            }
        }
    }

    // MARK: - Helpers

    func navigateBy(_ delta: Int) {
        guard let idx = currentIndex else { return }
        let newIdx = idx + delta
        guard allFiles.indices.contains(newIdx) else { return }
        withAnimation(.easeInOut(duration: 0.15)) {
            viewModel.previewFile = allFiles[newIdx]
            showEXIF = false
            exifData = [:]
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
                    result["Altitude"] = gps[kCGImagePropertyGPSAltitude].map { String(format: "%.0f m", ($0 as! Double)) } ?? "–"
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
                      "GPS", "Altitude", "Software"]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header
            HStack {
                Image(systemName: "info.circle.fill")
                    .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0))
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
                        // Show in defined order, then any extras
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

            // File path at bottom
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

struct VideoPlayerView: View {
    let url: URL

    var body: some View {
        VideoPlayer(player: AVPlayer(url: url))
            .clipShape(RoundedRectangle(cornerRadius: 12))
    }
}

// MARK: - QuickLookPreviewView (unchanged)

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

// MARK: - AsyncImageView (unchanged)

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
                    .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task {
            let fetchedImage = await Task.detached(priority: .userInitiated) { () -> NSImage? in
                if let data = try? Data(contentsOf: url, options: .mappedIfSafe) {
                    return NSImage(data: data)
                }
                return nil
            }.value

            await MainActor.run {
                if let i = fetchedImage { loadedImage = i } else { hasError = true }
            }
        }
    }
}
