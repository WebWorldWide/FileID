import Foundation
import Testing
@testable import FileID

@Suite("Engine pipe")
struct SigpipeTests {
    @Test func writingToAnExitedEngineFailsInsteadOfKillingTheApp() throws {
        let pipe = Pipe()
        EngineClient.refuseSigpipe(on: pipe)
        try pipe.fileHandleForReading.close()
        #expect(throws: (any Error).self) { try pipe.fileHandleForWriting.write(contentsOf: Data("{}\n".utf8)) }
    }
}
