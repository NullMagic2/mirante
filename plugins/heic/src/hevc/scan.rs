//! Module: mirante_format_heic::hevc::scan
//! Purpose: The up-right diagonal, horizontal and vertical scan orders (H.265 6.5.3-6.5.5).
//! Created: 2026-11-16
//! Architecture: Built at compile time; residual coding walks sub-blocks and
//! coefficients with them, scaling lists use the diagonal order.

/// Scan type as signalled by scanIdx.
pub const DIAGONAL: usize = 0;
pub const HORIZONTAL: usize = 1;
pub const VERTICAL: usize = 2;

/// Positions (x, y) in scan order for a block of `1 << log2` samples per side.
pub type ScanTable = [(u8, u8); 64];

/// SCANS[log2 size][scanIdx] for block sizes 1, 2, 4 and 8.
pub static SCANS: [[ScanTable; 3]; 4] = [
    [diagonal(1), horizontal(1), vertical(1)],
    [diagonal(2), horizontal(2), vertical(2)],
    [diagonal(4), horizontal(4), vertical(4)],
    [diagonal(8), horizontal(8), vertical(8)],
];

const fn diagonal(size: usize) -> ScanTable {
    let mut table = [(0, 0); 64];
    let mut index = 0;
    let mut line = 0;
    while index < size * size {
        let mut x = 0;
        let mut y = line as isize;
        while y >= 0 {
            if x < size && (y as usize) < size {
                table[index] = (x as u8, y as u8);
                index += 1;
            }
            x += 1;
            y -= 1;
        }
        line += 1;
    }
    table
}

const fn horizontal(size: usize) -> ScanTable {
    let mut table = [(0, 0); 64];
    let mut index = 0;
    while index < size * size {
        table[index] = ((index % size) as u8, (index / size) as u8);
        index += 1;
    }
    table
}

const fn vertical(size: usize) -> ScanTable {
    let mut table = [(0, 0); 64];
    let mut index = 0;
    while index < size * size {
        table[index] = ((index / size) as u8, (index % size) as u8);
        index += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagonal_scan_goes_up_and_right() {
        let scan = &SCANS[2][DIAGONAL];
        assert_eq!(scan[..6], [(0, 0), (0, 1), (1, 0), (0, 2), (1, 1), (2, 0)]);
        assert_eq!(scan[15], (3, 3));
        assert_eq!(SCANS[2][VERTICAL][1], (0, 1));
    }
}
