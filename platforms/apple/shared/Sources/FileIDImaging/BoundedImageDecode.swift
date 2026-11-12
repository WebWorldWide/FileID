import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

/// Bounded working image for the scan. For RAW files, uses an embedded preview
/// when it is at least two-thirds of the requested size (RAW files carry
/// full-size JPEG previews; decoding those is 10–17× cheaper than demosaicing
/// the sensor data), otherwise decodes the full image. Small EXIF thumbnails
/// never pass. A preview up to a third larger than the target is used at its
/// native size: resampling a 1616 px Sony preview down to 1536 px cost more
/// than decoding it. Larger previews are scaled to the target.
///
/// Other formats decode straight to the target. The preview probe made ImageIO
/// scale a camera JPEG's full image to the probe size first, so every photo was
/// decoded twice (73 vs 26 ms CPU per 24 MP JPEG, measured).
package func decodeBoundedImage(_ source: CGImageSource, maxPixelSize: Int) -> CGImage? {
    let cap = maxPixelSize * 4 / 3
    var options: [CFString: Any] = [
        kCGImageSourceShouldCacheImmediately: true,
        kCGImageSourceCreateThumbnailWithTransform: true,
        kCGImageSourceThumbnailMaxPixelSize: cap,
        kCGImageSourceCreateThumbnailFromImageIfAbsent: false
    ]
    guard isCameraRaw(source) else {
        options[kCGImageSourceCreateThumbnailFromImageIfAbsent] = nil
        options[kCGImageSourceCreateThumbnailFromImageAlways] = true
        options[kCGImageSourceThumbnailMaxPixelSize] = maxPixelSize
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }
    var largePreview = false
    if let preview = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) {
        let side = max(preview.width, preview.height)
        if side * 3 >= maxPixelSize * 2 && side < cap { return preview }
        if longSide(source).map({ $0 > side }) == false { return preview }
        largePreview = side >= cap
    }
    options[kCGImageSourceThumbnailMaxPixelSize] = maxPixelSize
    if largePreview {
        options[kCGImageSourceCreateThumbnailFromImageIfAbsent] = true
    } else {
        options[kCGImageSourceCreateThumbnailFromImageIfAbsent] = nil
        options[kCGImageSourceCreateThumbnailFromImageAlways] = true
    }
    return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
}

/// The same bounded decode, sending HEIC/HEIF through `HEIFDecoder` first
/// (hardware HEVC, about 4x ImageIO's throughput). Falls back to ImageIO for
/// layouts the decoder declines.
package func decodeBoundedImage(_ source: CGImageSource, path: String, maxPixelSize: Int) -> CGImage? {
    if isHEIF(source), let image = HEIFDecoder.decode(path: path, maxPixelSize: maxPixelSize) { return image }
    return decodeBoundedImage(source, maxPixelSize: maxPixelSize)
}

/// The scan's working image. Camera JPEGs that carry a large preview get the
/// same treatment as camera RAW files: the preview stands in for the full image
/// (a 33 MP Sony JPEG decoded at 37 photos/s in the hardware decoder, its
/// 1616 px preview at 182/s). Other callers keep decoding the full image.
package func decodeScanImage(_ source: CGImageSource, path: String, maxPixelSize: Int) -> CGImage? {
    if isJPEG(source), let image = JPEGPreview.decode(source, path: path, maxPixelSize: maxPixelSize) { return image }
    return decodeBoundedImage(source, path: path, maxPixelSize: maxPixelSize)
}

package func isJPEG(_ source: CGImageSource) -> Bool {
    guard let identifier = CGImageSourceGetType(source) as String?,
          let type = UTType(identifier) else { return false }
    return type.conforms(to: .jpeg)
}

// public.heic and public.heif are siblings, not parent and child.
package func isHEIF(_ source: CGImageSource) -> Bool {
    guard let identifier = CGImageSourceGetType(source) as String?,
          let type = UTType(identifier) else { return false }
    return type.conforms(to: .heic) || type.conforms(to: .heif)
}

private func isCameraRaw(_ source: CGImageSource) -> Bool {
    guard let identifier = CGImageSourceGetType(source) as String?,
          let type = UTType(identifier) else { return false }
    return type.conforms(to: .rawImage)
}

private func longSide(_ source: CGImageSource) -> Int? {
    guard let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
          let width = properties[kCGImagePropertyPixelWidth] as? Int,
          let height = properties[kCGImagePropertyPixelHeight] as? Int else { return nil }
    return max(width, height)
}
