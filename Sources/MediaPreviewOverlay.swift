import SwiftUI
import QuickLookUI
import AVKit

struct MediaPreviewOverlay: View {
    @ObservedObject var viewModel: AppViewModel
    
    var body: some View {
        if let file = viewModel.previewFile {
            ZStack {
                Color.black.opacity(0.85)
                    .ignoresSafeArea()
                    .onTapGesture {
                        withAnimation { viewModel.previewFile = nil }
                    }
                
                VStack {
                    HStack {
                        Spacer()
                        Button {
                            withAnimation { viewModel.previewFile = nil }
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                                .font(.system(size: 32))
                                .foregroundStyle(Color(red: 1.0, green: 0.8, blue: 0.0), .white.opacity(0.2))
                        }
                        .buttonStyle(.plain)
                        .padding()
                    }
                    
                    let ext = file.url.pathExtension.lowercased()
                    if ["mp4", "mov", "pdf"].contains(ext) {
                        QuickLookPreviewView(url: file.url)
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                            .clipShape(RoundedRectangle(cornerRadius: 16))
                            .padding()
                    } else {
                        AsyncImageView(url: file.url)
                    }
                    
                    Text(file.filename)
                        .font(.headline)
                        .foregroundStyle(.white)
                        .padding(.bottom)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .transition(.opacity)
            .zIndex(100)
        }
    }
}

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
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                    .padding()
            } else if hasError {
                Text("Unable to load image preview")
                    .foregroundStyle(.red)
            } else {
                ProgressView()
                    .controlSize(.large)
                    .tint(Color(red: 1.0, green: 0.8, blue: 0.0))
            }
        }
        .task {
            // Load massive files off the Main Thread securely
            let fetchedImage = await Task.detached(priority: .userInitiated) { () -> NSImage? in
                if let data = try? Data(contentsOf: url, options: .mappedIfSafe) {
                    return NSImage(data: data)
                }
                return nil
            }.value
            
            await MainActor.run {
                if let i = fetchedImage {
                    self.loadedImage = i
                } else {
                    self.hasError = true
                }
            }
        }
    }
}
