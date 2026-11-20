import Foundation

enum Plural {
    static func noun<Count: BinaryInteger>(_ count: Count, _ one: String, _ many: String) -> String {
        "\(count.formatted()) \(count == 1 ? one : many)"
    }
}
