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
            image = await Self.load(retryDelays: Self.retryDelays) { await ThumbnailService.shared.thumbnail(for: url, size: size) }
        }
        .onDisappear { image = nil }
    }

    static var retryDelays: [Duration] { [.seconds(5), .seconds(20), .seconds(60)] }

    /// A busy engine or a slow drive can miss the reply window, so a tile that comes back empty asks again after each
    /// delay while it stays on screen, and settles for the placeholder only when every ask came back empty.
    static func load(retryDelays: [Duration], fetch: () async -> NSImage?) async -> NSImage? {
        if let image = await fetch() { return image }
        for delay in retryDelays {
            guard !Task.isCancelled, (try? await Task.sleep(for: delay)) != nil else { return nil }
            if let image = await fetch() { return image }
        }
        return nil
    }
}
