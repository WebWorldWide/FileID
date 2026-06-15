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
        _ s: String, imageWidth: Int, imageHeight: Int
    ) -> (x: Double, y: Double, w: Double, h: Double)? {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return nil }

        if t.hasPrefix("{") {
            // Windows JSON: pixels, top-left origin.
            guard imageWidth > 0, imageHeight > 0,
                  let data = t.data(using: .utf8),
                  let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let px = numeric(obj["x"]), let py = numeric(obj["y"]),
                  let pw = numeric(obj["w"]), let ph = numeric(obj["h"]) else { return nil }
            let w = pw / Double(imageWidth)
            let h = ph / Double(imageHeight)
            let x = px / Double(imageWidth)
            // top-left → bottom-left (macOS/Vision convention).
            let yBottom = 1.0 - (py / Double(imageHeight)) - h
            return (x, yBottom, w, h)
        }

        // macOS CSV: normalized, bottom-left — passthrough (byte-identical to the
        // prior `split(",").compactMap(Double.init)` parse).
        let parts = t.split(separator: ",").compactMap { Double($0) }
        guard parts.count >= 4 else { return nil }
        return (parts[0], parts[1], parts[2], parts[3])
    }

    private static func numeric(_ any: Any?) -> Double? {
        if let n = any as? NSNumber { return n.doubleValue }
        if let d = any as? Double { return d }
        if let i = any as? Int { return Double(i) }
        return nil
    }
}
