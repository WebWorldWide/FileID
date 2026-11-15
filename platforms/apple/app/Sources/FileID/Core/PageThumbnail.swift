import CoreGraphics

/// Quick Look renders a document as a portrait page. A centred crop of that page into a
/// landscape tile lands on empty paper, so tiles show the top of the page instead, and
/// a page with no ink at all gets a glyph placeholder rather than a white rectangle.
enum PageThumbnail {
    static let tileAspect: CGFloat = 4.0 / 3.0

    /// `CALayer.contentsRect` (origin bottom-left) showing the top of a portrait page; nil when the whole image should fill the tile.
    static func topCrop(width: Int, height: Int) -> CGRect? {
        guard width > 0, height > 0 else { return nil }
        let aspect = CGFloat(width) / CGFloat(height)
        guard aspect < tileAspect * 0.85 else { return nil }
        let visible = min(1, aspect / tileAspect)
        return CGRect(x: 0, y: 1 - visible, width: 1, height: visible)
    }

    /// `contentsRect` showing the top-left of an image that is wider or taller than the tile, so the start of its text survives.
    static func topLeftCrop(width: Int, height: Int) -> CGRect? {
        guard width > 0, height > 0 else { return nil }
        let aspect = CGFloat(width) / CGFloat(height)
        if aspect > tileAspect * 1.05 { return CGRect(x: 0, y: 0, width: tileAspect / aspect, height: 1) }
        if aspect < tileAspect * 0.95 { return CGRect(x: 0, y: 1 - aspect / tileAspect, width: 1, height: aspect / tileAspect) }
        return nil
    }

    /// Screenshots and scanned documents: images whose content starts at the top-left.
    static func showsTopLeft(kind: String, hasText: Bool, tags: [String], fileName: String) -> Bool {
        guard kind == "image" else { return false }
        let name = fileName.lowercased()
        if name.hasPrefix("screenshot") || name.hasPrefix("screen shot") { return true }
        return hasText && tags.contains { ["document", "screenshot", "page", "text"].contains($0.lowercased()) }
    }

    /// True when the visible part of the page holds no marks: near-white (or fully transparent) everywhere.
    static func isBlank(_ image: CGImage, crop: CGRect?) -> Bool {
        let side = 64
        var pixels = [UInt8](repeating: 0, count: side * side * 4)
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let context = CGContext(data: &pixels, width: side, height: side, bitsPerComponent: 8, bytesPerRow: side * 4,
                                      space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
        context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: side, height: side))
        let visible = crop ?? CGRect(x: 0, y: 0, width: 1, height: 1)
        let w = CGFloat(image.width), h = CGFloat(image.height)
        let source = CGRect(x: visible.minX * w, y: (1 - visible.maxY) * h, width: visible.width * w, height: visible.height * h).integral
        guard let part = image.cropping(to: source) else { return false }
        context.interpolationQuality = .medium
        context.draw(part, in: CGRect(x: 0, y: 0, width: side, height: side))
        var marked = 0
        for i in stride(from: 0, to: pixels.count, by: 4) where pixels[i] < 248 || pixels[i + 1] < 248 || pixels[i + 2] < 248 { marked += 1 }
        return marked == 0
    }
}
