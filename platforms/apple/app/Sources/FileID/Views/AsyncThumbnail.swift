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
        .task(id: url) {
            let (url, size) = (url, size)
            image = await Self.load(retryAfter: .seconds(5)) { await ThumbnailService.shared.thumbnail(for: url, size: size) }
        }
        .onDisappear { image = nil }
    }

    /// A busy engine can miss its reply window for a large video, so a tile that comes back empty asks once more before it settles for the placeholder.
    static func load(retryAfter delay: Duration, fetch: () async -> NSImage?) async -> NSImage? {
        if let image = await fetch() { return image }
        guard !Task.isCancelled, (try? await Task.sleep(for: delay)) != nil else { return nil }
        return await fetch()
    }
}
