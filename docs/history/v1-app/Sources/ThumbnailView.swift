import SwiftUI

/// Async thumbnail loader. Calls `ThumbnailService.shared` (QuickLook-backed,
/// 500-item NSCache) and renders a placeholder until the image arrives.
struct ThumbnailView: View {
    let url: URL

    @State private var image: NSImage?

    var body: some View {
        ZStack {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFill()
            } else {
                Rectangle()
                    .fill(.quaternary)
                    .overlay(
                        Image(systemName: "photo")
                            .foregroundStyle(.tertiary)
                    )
            }
        }
        .task(id: url) {
            image = await ThumbnailService.shared.getThumbnail(for: url)
        }
    }
}
