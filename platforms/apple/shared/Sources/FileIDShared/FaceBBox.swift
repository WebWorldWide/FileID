// Cross-platform face-bbox parsing (read-tolerance). The two engines WRITE
// different bbox formats and neither converts the other's:
//   • macOS (Vision): "x,y,w,h" — NORMALIZED [0,1], BOTTOM-LEFT origin.
//   • Windows (SCRFD/YuNet): JSON {"x","y","w","h",roll,yaw,pitch} — PIXELS in the
//     original image, TOP-LEFT origin.
// A library scanned on one OS and opened on the other therefore had its faces
// fail to crop (the foreign parser returned nil → face excluded / blank crop).
// This parses BOTH into the macOS canonical form (normalized, bottom-left) so the
// macOS crop consumers work on a Windows-scanned library too. Each engine still
// WRITES its own native format — this is read-tolerance only, so within-platform
// behavior is byte-identical (the CSV branch is the exact prior logic).
//
// Windows itself never reads bbox back for cropping (it saves face-crop JPEGs at
// scan time and clusters from embeddings), so the reverse direction needs no
// change — this is macOS-side only.
import Foundation

public enum FaceBBox {
    /// Parse a stored face bbox (either format) into NORMALIZED, BOTTOM-LEFT
    /// (x, y, w, h) — the macOS canonical form. `imageWidth/Height` are needed to
    /// normalize + flip the Windows pixel/top-left form; they're ignored for the
    /// already-normalized macOS CSV form. Returns nil on a malformed string.
    public static func parseNormalized(
        _ s: String, imageWidth: Int, imageHeight: Int, sourceOrientation: Int = 1
    ) -> (x: Double, y: Double, w: Double, h: Double)? {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return nil }

        if t.hasPrefix("{") {
            // Windows JSON: pixels, top-left origin.
            guard imageWidth > 0, imageHeight > 0,
                  let data = t.data(using: .utf8),
                  let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let px = numeric(obj["x"]), let py = numeric(obj["y"]),
                  let pw = numeric(obj["w"]), let ph = numeric(obj["h"]),
                  [px, py, pw, ph].allSatisfy(\.isFinite), pw > 0, ph > 0 else { return nil }
            let sourceWidth: Double
            let sourceHeight: Double
            if obj["sourceWidth"] != nil || obj["sourceHeight"] != nil {
                guard obj["coordinateSpace"] as? String == "pixel-top-left",
                      let width = numeric(obj["sourceWidth"]), let height = numeric(obj["sourceHeight"]),
                      width.isFinite, height.isFinite, width > 0, height > 0 else { return nil }
                sourceWidth = width
                sourceHeight = height
            } else {
                sourceWidth = Double(imageWidth)
                sourceHeight = Double(imageHeight)
            }
            guard (1...8).contains(sourceOrientation) else { return nil }
            if sourceOrientation != 1 && obj["sourceWidth"] == nil { return nil }
            let x = px / sourceWidth
            let y = py / sourceHeight
            let w = pw / sourceWidth
            let h = ph / sourceHeight
            if sourceOrientation == 1 { return (x, 1 - y - h, w, h) }
            func oriented(_ x: Double, _ y: Double) -> (Double, Double) {
                switch sourceOrientation {
                case 2: return (1 - x, y)
                case 3: return (1 - x, 1 - y)
                case 4: return (x, 1 - y)
                case 5: return (y, x)
                case 6: return (1 - y, x)
                case 7: return (1 - y, 1 - x)
                case 8: return (y, 1 - x)
                default: return (x, y)
                }
            }
            let corners = [oriented(x,y),oriented(x+w,y),oriented(x,y+h),oriented(x+w,y+h)]
            guard let minX = corners.map(\.0).min(), let maxX = corners.map(\.0).max(),
                  let minY = corners.map(\.1).min(), let maxY = corners.map(\.1).max()
            else { return nil }
            return (minX, 1 - maxY, maxX - minX, maxY - minY)
        }

        // macOS CSV: normalized, bottom-left — passthrough (byte-identical to the
        // prior `split(",").compactMap(Double.init)` parse).
        let fields = t.split(separator: ",", omittingEmptySubsequences: false)
        guard fields.count >= 4 else { return nil }
        let parts = fields.prefix(4).compactMap { Double($0) }
        guard parts.count == 4, parts.allSatisfy(\.isFinite), parts[2] > 0, parts[3] > 0 else { return nil }
        return (parts[0], parts[1], parts[2], parts[3])
    }

    private static func numeric(_ any: Any?) -> Double? {
        if let n = any as? NSNumber { return n.doubleValue }
        if let d = any as? Double { return d }
        if let i = any as? Int { return Double(i) }
        return nil
    }
}
