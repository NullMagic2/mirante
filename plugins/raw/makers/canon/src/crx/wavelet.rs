//! Module: mirante_raw_canon::crx::wavelet
//! Purpose: Inverse reversible 5/3 (LeGall) wavelet, as used by lossy CRX.
//! Created: 2026-11-16
//! Architecture: Integer lifting with whole-sample symmetric edges, rows first
//! then columns, which is the order Canon's (and LibRaw's) decoder uses; the
//! integer rounding makes the order matter.

/// A row-major block of coefficients.
#[derive(Clone, Debug, Default)]
pub struct Block {
    pub width: usize,
    pub height: usize,
    pub values: Vec<i32>,
}

impl Block {
    pub fn row(&self, y: usize) -> &[i32] {
        &self.values[y * self.width..][..self.width]
    }
}

/// Rebuilds `low.len() + high.len()` samples from interleaved lifting steps.
fn lift_line(low: &[i32], high: &[i32], out: &mut [i32]) {
    if high.is_empty() {
        out[0] = low[0];
        return;
    }
    let last_high = high.len() - 1;
    for (k, &value) in low.iter().enumerate() {
        let left = high[k.saturating_sub(1)];
        let right = high[k.min(last_high)];
        out[2 * k] = value.wrapping_sub(left.wrapping_add(right).wrapping_add(2) >> 2);
    }
    for (k, &value) in high.iter().enumerate() {
        let next = out.get(2 * k + 2).copied().unwrap_or(out[2 * k]);
        out[2 * k + 1] = value.wrapping_add(out[2 * k].wrapping_add(next) >> 1);
    }
}

/// One inverse level: LL with HL (high columns), LH (high rows) and HH.
/// Returns None when the four bands do not fit together.
pub fn inverse_level(ll: &Block, hl: &Block, lh: &Block, hh: &Block) -> Option<Block> {
    let fits = hl.height == ll.height
        && lh.width == ll.width
        && (hh.width, hh.height) == (hl.width, lh.height)
        && matches!(ll.width - hl.width.min(ll.width), 0 | 1)
        && matches!(ll.height - lh.height.min(ll.height), 0 | 1)
        && hl.width <= ll.width
        && lh.height <= ll.height
        && ll.width > 0
        && ll.height > 0;
    if !fits {
        return None;
    }
    let width = ll.width + hl.width;
    let height = ll.height + lh.height;
    let horizontal = |low: &Block, high: &Block| -> Vec<i32> {
        let mut rows = vec![0; width * low.height];
        for (y, out) in rows.chunks_exact_mut(width).enumerate() {
            lift_line(low.row(y), high.row(y), out);
        }
        rows
    };
    let low_rows = horizontal(ll, hl);
    let high_rows = horizontal(lh, hh);
    let mut values = vec![0; width * height];
    if lh.height == 0 {
        values.copy_from_slice(&low_rows);
        return Some(Block { width, height, values });
    }
    // The same lifting down the columns, a whole row at a time.
    let low_row = |k: usize| &low_rows[k * width..][..width];
    let high_row = |k: usize| &high_rows[k.min(lh.height - 1) * width..][..width];
    for k in 0..ll.height {
        let (above, below) = (high_row(k.saturating_sub(1)), high_row(k));
        let out = &mut values[2 * k * width..][..width];
        for (x, target) in out.iter_mut().enumerate() {
            let correction = above[x].wrapping_add(below[x]).wrapping_add(2) >> 2;
            *target = low_row(k)[x].wrapping_sub(correction);
        }
    }
    for k in 0..lh.height {
        let next = if 2 * k + 2 < height { 2 * k + 2 } else { 2 * k };
        for x in 0..width {
            let sum = values[2 * k * width + x].wrapping_add(values[next * width + x]);
            values[(2 * k + 1) * width + x] = high_row(k)[x].wrapping_add(sum >> 1);
        }
    }
    Some(Block { width, height, values })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forward 5/3 lifting with the same edges, to check the inverse.
    fn forward_line(x: &[i32]) -> (Vec<i32>, Vec<i32>) {
        let n = x.len();
        let at = |i: usize| x[if i < n { i } else { 2 * (n - 1) - i }];
        let high: Vec<i32> =
            (0..n / 2).map(|k| at(2 * k + 1) - ((at(2 * k) + at(2 * k + 2)) >> 1)).collect();
        let low = (0..n.div_ceil(2))
            .map(|k| {
                let left = high[k.saturating_sub(1)];
                let right = high[k.min(high.len() - 1)];
                x[2 * k] + ((left + right + 2) >> 2)
            })
            .collect();
        (low, high)
    }

    #[test]
    fn inverts_lines_of_any_parity() {
        for n in 2..9 {
            let x: Vec<i32> = (0..n).map(|i| (i * 37 % 11) as i32 - 5).collect();
            let (low, high) = forward_line(&x);
            let mut out = vec![0; n];
            lift_line(&low, &high, &mut out);
            assert_eq!(out, x, "length {n}");
        }
    }

    #[test]
    fn inverts_a_two_dimensional_level() {
        let (width, height) = (5, 3);
        let image: Vec<i32> = (0..width * height).map(|i| (i * 53 % 17) as i32).collect();
        // The inverse lifts rows, then columns, so the forward pass goes columns first.
        let mut column_low = vec![vec![0; width]; height.div_ceil(2)];
        let mut column_high = vec![vec![0; width]; height / 2];
        for x in 0..width {
            let column: Vec<i32> = (0..height).map(|y| image[y * width + x]).collect();
            let (low, high) = forward_line(&column);
            low.iter().enumerate().for_each(|(y, &v)| column_low[y][x] = v);
            high.iter().enumerate().for_each(|(y, &v)| column_high[y][x] = v);
        }
        let split = |rows: &[Vec<i32>]| {
            let (mut low, mut high) = (Block::default(), Block::default());
            for row in rows {
                let (l, h) = forward_line(row);
                (low.width, high.width) = (l.len(), h.len());
                low.values.extend(l);
                high.values.extend(h);
            }
            (low.height, high.height) = (rows.len(), rows.len());
            (low, high)
        };
        let (ll, hl) = split(&column_low);
        let (lh, hh) = split(&column_high);
        let block = inverse_level(&ll, &hl, &lh, &hh).unwrap();
        assert_eq!((block.width, block.height), (width, height));
        assert_eq!(block.values, image);
    }
}
