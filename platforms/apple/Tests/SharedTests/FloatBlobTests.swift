import Foundation
import Testing
import FileIDShared

@Suite("Float blob codec")
struct FloatBlobTests {
    @Test("round-trips, ignores a trailing partial float, and decodes empty input to nothing")
    func roundTrip() {
        let values: [Float] = [1, -0.5, .pi, 0, 1e-7]
        let blob = FloatBlob.encode(values)
        #expect(blob.count == values.count * 4)
        #expect(FloatBlob.decode(blob) == values)
        #expect(FloatBlob.decode(blob + Data([0xFF, 0xFF])) == values)
        #expect(FloatBlob.decode(Data()).isEmpty)
        #expect(FloatBlob.decode(Data([1, 2, 3])).isEmpty)
        #expect(FloatBlob.decode(blob.dropFirst(4)) == Array(values.dropFirst()))
    }
}
