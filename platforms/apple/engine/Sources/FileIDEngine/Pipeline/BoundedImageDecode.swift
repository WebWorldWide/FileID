import CoreGraphics
import ImageIO

/// Bounded working image for the scan. Uses an embedded preview when it is at
/// least two-thirds of the requested size (RAW files carry full-size JPEG
/// previews; decoding those is 10–17× cheaper than demosaicing the sensor
/// data), otherwise decodes the full image. Small EXIF thumbnails never pass.
func decodeBoundedImage(_ source: CGImageSource, maxPixelSize: Int) -> CGImage? {
    var options: [CFString: Any] = [
        kCGImageSourceShouldCacheImmediately: true,
        kCGImageSourceCreateThumbnailWithTransform: true,
        kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
        kCGImageSourceCreateThumbnailFromImageIfAbsent: true
    ]
    if let preview = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary),
       max(preview.width, preview.height) * 3 >= maxPixelSize * 2 || !hasLargerImage(source, than: preview) {
        return preview
    }
    options[kCGImageSourceCreateThumbnailFromImageIfAbsent] = nil
    options[kCGImageSourceCreateThumbnailFromImageAlways] = true
    return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
}

private func hasLargerImage(_ source: CGImageSource, than image: CGImage) -> Bool {
    guard let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
          let width = properties[kCGImagePropertyPixelWidth] as? Int,
          let height = properties[kCGImagePropertyPixelHeight] as? Int else { return true }
    return max(width, height) > max(image.width, image.height)
}
