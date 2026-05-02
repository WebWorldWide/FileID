import Foundation

/// SF Symbol name for a Restructure destination bucket.
/// Shared by SankeyFlowView, TreeDiffView, and RestructureView.
func bucketIconName(_ bucket: String) -> String {
    if bucket.hasPrefix("People")    { return "person.crop.circle.fill" }
    if bucket.hasPrefix("Places")    { return "mappin.circle.fill" }
    if bucket.hasPrefix("Documents") { return "doc.text.fill" }
    if bucket.hasPrefix("Photos")    { return "photo.stack.fill" }
    return "tray.fill"
}
