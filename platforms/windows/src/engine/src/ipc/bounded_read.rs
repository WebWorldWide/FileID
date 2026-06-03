//! Bounded line reads + resync helpers for the stdin IPC loop.
//!
//! We don't use `BufReader::lines()` / `next_line()` because those buffer
//! the whole line before returning, which lets a hostile no-newline blob
//! OOM the engine before any cap fires. Instead, this module reads byte
//! by byte (well, in 8 KB chunks via the BufReader's internal buffer) and
//! rejects the moment the in-progress line crosses `max_bytes`.

use tokio::io::{AsyncBufRead, AsyncReadExt};

/// Outcome of one stdin line read.
pub(crate) enum BoundedRead {
    Line(String),
    Oversized(usize),
    Eof,
}

/// Read up to `max_bytes` of one newline-delimited frame from `reader`.
/// The supplied `buf` is cleared at entry and reused across calls so the
/// hot path doesn't allocate.
pub(crate) async fn bounded_read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<BoundedRead> {
    buf.clear();
    let mut byte = [0u8; 1];
    loop {
        match reader.read_exact(&mut byte).await {
            Ok(_) => {
                if byte[0] == b'\n' {
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                    // Build the String from a borrow, NOT `mem::take(buf)`:
                    // taking swaps the caller's pre-grown 8 KB Vec out for an
                    // empty one, so every frame after the first re-grows from
                    // capacity 0 — defeating the documented zero-alloc reuse.
                    // `buf` is cleared at the top of the next call.
                    let text = String::from_utf8_lossy(buf).into_owned();
                    return Ok(BoundedRead::Line(text));
                }
                if buf.len() >= max_bytes {
                    return Ok(BoundedRead::Oversized(buf.len()));
                }
                buf.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                if buf.is_empty() {
                    return Ok(BoundedRead::Eof);
                }
                // Trailing partial line at EOF — treat as a complete frame.
                let text = String::from_utf8_lossy(buf).into_owned();
                return Ok(BoundedRead::Line(text));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Drain bytes from `reader` until the next newline. Used to resync the IPC
/// framing after rejecting an oversized frame. Best-effort; swallows errors
/// and returns on any failure or EOF.
pub(crate) async fn drain_to_newline<R: AsyncBufRead + Unpin>(reader: &mut R) {
    let mut byte = [0u8; 1];
    while reader.read_exact(&mut byte).await.is_ok() {
        if byte[0] == b'\n' {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    fn br(bytes: &'static [u8]) -> BufReader<&'static [u8]> {
        BufReader::new(bytes)
    }

    #[tokio::test]
    async fn reads_a_simple_line() {
        let mut r = br(b"hello\nworld\n");
        let mut buf = Vec::new();
        let out = bounded_read_line(&mut r, &mut buf, 1024).await.unwrap();
        match out {
            BoundedRead::Line(t) => assert_eq!(t, "hello"),
            _ => panic!("expected Line"),
        }
    }

    #[tokio::test]
    async fn strips_trailing_cr_before_lf() {
        let mut r = br(b"hello\r\n");
        let mut buf = Vec::new();
        let out = bounded_read_line(&mut r, &mut buf, 1024).await.unwrap();
        match out {
            BoundedRead::Line(t) => assert_eq!(t, "hello"),
            _ => panic!("expected Line"),
        }
    }

    #[tokio::test]
    async fn returns_eof_on_clean_close() {
        let mut r = br(b"");
        let mut buf = Vec::new();
        let out = bounded_read_line(&mut r, &mut buf, 1024).await.unwrap();
        assert!(matches!(out, BoundedRead::Eof));
    }

    #[tokio::test]
    async fn trailing_partial_line_becomes_line_at_eof() {
        let mut r = br(b"no-terminator");
        let mut buf = Vec::new();
        let out = bounded_read_line(&mut r, &mut buf, 1024).await.unwrap();
        match out {
            BoundedRead::Line(t) => assert_eq!(t, "no-terminator"),
            _ => panic!("expected Line at EOF"),
        }
    }

    #[tokio::test]
    async fn rejects_oversized_frame() {
        // 64 KB of "a" followed by a newline; cap at 1024.
        static BIG: &[u8] = &[b'a'; 65536];
        let mut r = br(BIG);
        let mut buf = Vec::new();
        let out = bounded_read_line(&mut r, &mut buf, 1024).await.unwrap();
        match out {
            BoundedRead::Oversized(seen) => assert!(seen >= 1024),
            _ => panic!("expected Oversized"),
        }
    }

    #[tokio::test]
    async fn drains_to_newline_resyncs_after_oversize() {
        // After an oversized frame is rejected, drain_to_newline should
        // skip ahead so the next bounded_read_line sees a fresh frame.
        let mut r = br(b"junkjunkjunk\nsecond\n");
        // Pretend we already consumed 12 bytes ("junkjunkjunk").
        // We test from a clean position: call drain then bounded_read.
        drain_to_newline(&mut r).await;
        let mut buf = Vec::new();
        let out = bounded_read_line(&mut r, &mut buf, 1024).await.unwrap();
        match out {
            BoundedRead::Line(t) => assert_eq!(t, "second"),
            _ => panic!("expected Line after drain"),
        }
    }
}
