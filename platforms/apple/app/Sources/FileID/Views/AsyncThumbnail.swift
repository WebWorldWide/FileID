import SwiftUI

/// Drops the image when the view leaves the screen; ThumbnailService keeps the cache.
struct AsyncThumbnail<Placeholder: View>: View {
    let url: URL
    let size: CGFloat
    let contentMode: ContentMode
    let placeholder: Placeholder
    @State private var image: NSImage?

    init(url: URL, size: CGFloat, contentMode: ContentMode = .fill, @ViewBuilder placeholder: () -> Placeholder) {
        self.url = url
        self.size = size
        self.contentMode = contentMode
        self.placeholder = placeholder()
    }

    var body: some View {
        ZStack {
            if let image {
                Image(nsImage: image).resizable().aspectRatio(contentMode: contentMode)
            } else {
                placeholder
            }
        }
        .task(id: url) { image = await ThumbnailService.shared.thumbnail(for: url, size: size) }
        .onDisappear { image = nil }
    }
}
