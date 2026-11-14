//! Dot product and squared distance over eight independent accumulators.
//! A single running `f32` sum cannot be reordered by the compiler, so it stays
//! scalar; eight lanes vectorise. The lane order is fixed, so results are
//! identical on every run and thread count.

const LANES: usize = 8;

#[inline]
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let (a, b) = (&a[..len], &b[..len]);
    let mut lanes = [0.0f32; LANES];
    let (a_chunks, a_tail) = a.as_chunks::<LANES>();
    let (b_chunks, b_tail) = b.as_chunks::<LANES>();
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for lane in 0..LANES {
            lanes[lane] += x[lane] * y[lane];
        }
    }
    let tail: f32 = a_tail.iter().zip(b_tail).map(|(x, y)| x * y).sum();
    lanes.iter().sum::<f32>() + tail
}

#[inline]
pub(crate) fn squared_l2(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let (a, b) = (&a[..len], &b[..len]);
    let mut lanes = [0.0f32; LANES];
    let (a_chunks, a_tail) = a.as_chunks::<LANES>();
    let (b_chunks, b_tail) = b.as_chunks::<LANES>();
    for (x, y) in a_chunks.iter().zip(b_chunks) {
        for lane in 0..LANES {
            let d = x[lane] - y[lane];
            lanes[lane] += d * d;
        }
    }
    let tail: f32 = a_tail.iter().zip(b_tail).map(|(x, y)| (x - y) * (x - y)).sum();
    lanes.iter().sum::<f32>() + tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(len: usize, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state % 2001) as f32 / 1000.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn lanes_agree_with_the_plain_sums_at_every_length() {
        for len in [0usize, 1, 7, 8, 9, 15, 16, 17, 128, 511, 512, 513] {
            let (a, b) = (sample(len, 3), sample(len, 11));
            let plain_dot: f64 = a.iter().zip(&b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum();
            let plain_l2: f64 = a.iter().zip(&b).map(|(x, y)| f64::from(x - y).powi(2)).sum();
            assert!((f64::from(dot(&a, &b)) - plain_dot).abs() < 1e-3, "dot at {len}");
            assert!((f64::from(squared_l2(&a, &b)) - plain_l2).abs() < 1e-3, "l2 at {len}");
        }
    }

    #[test]
    fn a_shorter_side_bounds_the_sum_like_zip() {
        let (a, b) = (sample(20, 5), sample(12, 9));
        assert_eq!(dot(&a, &b).to_bits(), dot(&a[..12], &b).to_bits());
        assert_eq!(squared_l2(&a, &b).to_bits(), squared_l2(&a[..12], &b).to_bits());
    }
}
