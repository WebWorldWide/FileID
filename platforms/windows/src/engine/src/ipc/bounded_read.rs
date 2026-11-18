//! Bounded line reads + resync helpers for the stdin IPC loop. `BufReader::lines()`/`next_line()` buffer the whole line
//! first, so a hostile no-newline blob could OOM the engine before any cap fires; this reads in BufReader-buffer chunks
//! and rejects as soon as the line crosses `max_bytes`.

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub(crate) enum BoundedRead {
    Line(String),
    Oversized(usize),
    Eof,
}

/// `buf` is cleared at entry and reused across calls so the hot path doesn't allocate.
pub(crate) async fn bounded_read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<BoundedRead> {
    buf.clear();
    loop {
        // Take the already-buffered slice in one shot (O(bytes / 8192) polls, not per byte); fill_buf is empty at EOF.
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(BoundedRead::Eof);
            }
            // A trailing partial line at EOF is a complete frame.
            let text = String::from_utf8_lossy(buf).into_owned();
            return Ok(BoundedRead::Line(text));
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(nl) => {
                // Reject before copying; leave the '
                // ' unconsumed so drain_to_newline can resync.
                if buf.len() + nl > max_bytes {
                    let seen = buf.len() + nl;
                    reader.consume(nl);
                    return Ok(BoundedRead::Oversized(seen));
                }
                buf.extend_from_slice(&available[..nl]);
                reader.consume(nl + 1);
                if buf.last() == Some(&b'\r') {
                    buf.pop();
                }
                // Build the String from a borrow, NOT `mem::take(buf)`: taking swaps out the caller's pre-grown Vec, so
                // every later frame would re-grow from capacity 0.
                let text = String::from_utf8_lossy(buf).into_owned();
                return Ok(BoundedRead::Line(text));
            }
            None => {
                let take = available.len();
                if buf.len() + take > max_bytes {
                    let seen = buf.len() + take;
                    reader.consume(take);
                    return Ok(BoundedRead::Oversized(seen));
                }
                buf.extend_from_slice(available);
                reader.consume(take);
            }
        }
    }
}

/// Best-effort resync after an oversized frame; swallows errors. Gives up after `MAX_DRAIN` bytes without a newline:
/// this runs outside the stdio loop's `select!`, so an unbounded drain of a newline-free multi-GB stream would pin the
/// loop and defeat shutdown.
pub(crate) async fn drain_to_newline<R: AsyncBufRead + Unpin>(reader: &mut R) {
    const MAX_DRAIN: usize = 64 * 1024 * 1024;
    let mut drained = 0usize;
    loop {
        let available = match reader.fill_buf().await {
            Ok(b) => b,
            Err(_) => return,
        };
        if available.is_empty() {
            return;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            reader.consume(pos + 1);
            return;
        }
        let len = available.len();
        reader.consume(len);
        drained = drained.saturating_add(len);
        if drained >= MAX_DRAIN {
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
        let mut r = br(b"junkjunkjunk\nsecond\n");
        drain_to_newline(&mut r).await;
        let mut buf = Vec::new();
        let out = bounded_read_line(&mut r, &mut buf, 1024).await.unwrap();
        match out {
            BoundedRead::Line(t) => assert_eq!(t, "second"),
            _ => panic!("expected Line after drain"),
        }
    }
}
