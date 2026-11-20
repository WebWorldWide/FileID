import Foundation
import Testing
import FileIDImaging

@Suite("Imaging log", .serialized)
struct ImagingLogTests {
    @Test("a stderr that cannot be written is ignored instead of raising an exception that ends the engine")
    func brokenStderrDoesNotCrash() {
        signal(SIGPIPE, SIG_IGN)
        var fds: [Int32] = [0, 0]
        #expect(pipe(&fds) == 0)
        close(fds[0])
        let saved = dup(STDERR_FILENO)
        dup2(fds[1], STDERR_FILENO)
        ImagingLog.info("polish_safe_closed_stderr", metrics: ["n": 1])
        ImagingLog.warn("polish_safe_closed_stderr", error: "disk full")
        dup2(saved, STDERR_FILENO)
        close(saved)
        close(fds[1])
    }
}
