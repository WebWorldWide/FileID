import Foundation

/// Per-face quality gate shared by People and any feature that ranks faces
/// (Best Takes). A face that fails is still detected and kept, but it is
/// "held": it never starts or bridges a person, and joins one only through
/// FaceClustering.attachHeldFaces. Inputs are Apple Vision measurements:
/// `quality` is faceCaptureQuality (nil when Vision did not measure it), pose
/// is in radians, `bboxArea` is a fraction of the image and `minDimPx` is the
/// short side of the box in decoded pixels.
public enum FaceQualityGate {
    public enum Reason: String, Sendable, Equatable {
        case unmeasured, lowQuality, tooSmall, profile, tilted
    }

    public static let qualityFloor: Double = 0.02
    public static let minBBoxAreaFraction: Double = 0.0002
    public static let minBBoxMinDimPx: Double = 40.0
    public static let maxYawRadians: Double = 50.0 * .pi / 180.0
    public static let maxPitchRadians: Double = 30.0 * .pi / 180.0

    /// nil when the face is good enough to define a person.
    public static func holdReason(quality: Double?, yaw: Double?, pitch: Double?,
                                  bboxArea: Double, minDimPx: Double) -> Reason? {
        guard let q = quality else { return .unmeasured }
        if q < qualityFloor { return .lowQuality }
        if bboxArea < minBBoxAreaFraction || minDimPx < minBBoxMinDimPx { return .tooSmall }
        if let y = yaw, abs(y) > maxYawRadians { return .profile }
        if let p = pitch, abs(p) > maxPitchRadians { return .tilted }
        return nil
    }
}
