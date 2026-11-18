import SwiftUI

/// A folder path that gives up whole folders when the room runs out ("…/Eckerts 10-4-26/Photos")
/// instead of cutting a name in half. The full path stays in the tooltip.
struct PathLabel: View {
    let path: String
    var font: Font
    var color: Color

    var body: some View {
        let options = PathText.shortenings(PathText.abbreviated(path))
        ViewThatFits(in: .horizontal) {
            ForEach(options, id: \.self) { option in
                Text(option).fixedSize(horizontal: true, vertical: false)
            }
            Text(options.last ?? path).truncationMode(.head)
        }
        .font(font)
        .foregroundStyle(color)
        .lineLimit(1)
        .help(path)
    }
}
