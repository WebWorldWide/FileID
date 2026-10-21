import Testing
import FileIDShared

@Suite("Duration text")
struct DurationTextTests {
    @Test("short form picks the two largest units and survives bad input")
    func short() {
        #expect(DurationText.short(9.4) == "9s")
        #expect(DurationText.short(252) == "4m 12s")
        #expect(DurationText.short(7_500) == "2h 5m")
        #expect(DurationText.short(1e9) == "99h 59m")
        #expect(DurationText.short(-3) == "0s")
        #expect(DurationText.short(.nan) == "—")
    }

    @Test("clock and spoken forms round down")
    func clock() {
        #expect(DurationText.clock(65.9) == "1:05")
        #expect(DurationText.clock(3_723) == "1:02:03")
        #expect(DurationText.clock(.infinity) == "0:00")
        #expect(DurationText.spoken(3_661) == "1 hour 1 minute 1 second")
        #expect(DurationText.spoken(2) == "2 seconds")
    }
}
