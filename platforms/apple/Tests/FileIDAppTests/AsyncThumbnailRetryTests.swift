import SwiftUI
import AppKit
import Testing
@testable import FileID

@Suite struct AsyncThumbnailRetryTests {
    @Test func aTileThatCameBackEmptyAsksAgainAndShowsTheSecondAnswer() async {
        let calls = Counter()
        let image = await AsyncThumbnail<EmptyView>.load(retryAfter: .milliseconds(1)) {
            await calls.bump() == 1 ? nil : NSImage(size: NSSize(width: 4, height: 4))
        }
        #expect(image != nil)
        #expect(await calls.value == 2)
    }

    @Test func aTileThatLoadsFirstTimeDoesNotAskTwice() async {
        let calls = Counter()
        _ = await AsyncThumbnail<EmptyView>.load(retryAfter: .milliseconds(1)) {
            _ = await calls.bump()
            return NSImage(size: NSSize(width: 4, height: 4))
        }
        #expect(await calls.value == 1)
    }
}

private actor Counter {
    private(set) var value = 0
    func bump() -> Int { value += 1; return value }
}
