// LineBuffer framing tests — newline-delimited IPC framing with a resume
// offset (R3-07B) so a large un-terminated frame arriving across many reads is
// scanned once, not re-scanned from the front each read. These assert the
// resume logic frames identically to a naive whole-buffer scan.
import Testing
import Foundation
@testable import FileIDShared

@Suite struct LineReaderTests {
    private func d(_ s: String) -> Data { Data(s.utf8) }

    @Test func singleCompleteLine() throws {
        let buf = LineBuffer()
        #expect(try buf.append(d("hello\n")) == [d("hello")])
    }

    @Test func multipleLinesInOneChunk() throws {
        let buf = LineBuffer()
        #expect(try buf.append(d("a\nbb\nccc\n")) == [d("a"), d("bb"), d("ccc")])
    }

    @Test func emptyLinesSkipped() throws {
        let buf = LineBuffer()
        // Blank lines (consecutive newlines) are dropped, non-empty kept.
        #expect(try buf.append(d("a\n\n\nb\n")) == [d("a"), d("b")])
    }

    @Test func partialLineCompletedAcrossChunks() throws {
        let buf = LineBuffer()
        #expect(try buf.append(d("abc")).isEmpty)
        #expect(try buf.append(d("def\n")) == [d("abcdef")])
    }

    @Test func completeLinePlusTrailingPartial() throws {
        let buf = LineBuffer()
        // "a\nb" → one line "a"; "b" stays buffered until a newline.
        #expect(try buf.append(d("a\nb")) == [d("a")])
        #expect(try buf.append(d("c\n")) == [d("bc")])
    }

    @Test func newlineExactlyAtChunkBoundary() throws {
        // The resume offset is set to the buffer length after the first
        // (newline-free) chunk; the newline then arrives as the first byte of
        // the next chunk — it must still be found at exactly that offset.
        let buf = LineBuffer()
        #expect(try buf.append(d("hello")).isEmpty)
        #expect(try buf.append(d("\nworld")) == [d("hello")])
        #expect(buf.flushAll() == d("world"))
    }

    @Test func resumeFramesIdenticallyAcrossManyChunks() throws {
        // The heart of the O(n²)→O(n) fix: a single long frame fed in 1000
        // newline-free chunks, terminated only at the end, must frame as one
        // line equal to the full concatenation — proving the resume offset
        // never drops bytes nor misses the terminator.
        let buf = LineBuffer()
        var expected = Data()
        for i in 0..<1000 {
            let chunk = d("chunk\(i)-")
            expected.append(chunk)
            #expect(try buf.append(chunk).isEmpty)
        }
        let lines = try buf.append(d("END\n"))
        expected.append(d("END"))
        #expect(lines == [expected])
    }

    @Test func multipleLinesAfterAccumulatedPartial() throws {
        // After resuming past an accumulated partial, a chunk carrying several
        // newlines must yield every complete line in order and keep the tail.
        let buf = LineBuffer()
        #expect(try buf.append(d("start")).isEmpty)
        #expect(try buf.append(d("-end\nsecond\nthi")) == [d("start-end"), d("second")])
        #expect(try buf.append(d("rd\n")) == [d("third")])
    }

    @Test func overflowThrowsThenRecovers() throws {
        // An un-terminated frame past the cap throws and resets the buffer, so
        // a subsequent well-formed line frames normally (the DoS guard).
        let buf = LineBuffer()
        let huge = Data(count: LineBuffer.maxLineBytes + 16) // zero bytes, no 0x0A
        #expect(throws: LineOverflowError.self) { try buf.append(huge) }
        #expect(try buf.append(d("recovered\n")) == [d("recovered")])
    }

    @Test func overflowMessageReflectsCap() {
        #expect(LineOverflowError(capBytes: 64 * 1024 * 1024).description == "IPC line exceeded 64 MiB cap")
    }
}
