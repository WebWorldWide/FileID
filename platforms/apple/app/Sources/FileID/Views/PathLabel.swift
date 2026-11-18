import SwiftUI

/// Drops whole folders to fit rather than cutting a name in half.
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
