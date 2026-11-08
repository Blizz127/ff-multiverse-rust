//! The tint palette of the character creation screen: sixteen bright colours in a grid, plus
//! helpers to find a colour on it.

/// The grid is `COLS` x `ROWS`, row-major.
pub const COLS: usize = 8;
pub const ROWS: usize = 2;

/// Bright, well separated colours: they are drawn as the player's marker over a dark field, so
/// none of them is dark.
pub const PALETTE: [[u8; 3]; COLS * ROWS] = [
    [235, 80, 80],   // red
    [245, 150, 60],  // orange
    [245, 215, 70],  // yellow
    [160, 225, 75],  // lime
    [70, 205, 105],  // green
    [60, 205, 190],  // teal
    [85, 195, 245],  // sky
    [90, 125, 245],  // blue
    [140, 105, 245], // indigo
    [190, 100, 235], // purple
    [235, 85, 205],  // magenta
    [250, 150, 185], // pink
    [245, 245, 245], // white
    [170, 178, 190], // silver
    [180, 130, 85],  // brown
    [255, 190, 40],  // gold
];

/// Grid position (column, row) of palette entry `index`.
pub fn cell(index: usize) -> (usize, usize) {
    (index % COLS, index / COLS)
}

/// Palette index at grid position (`col`, `row`); out-of-range positions are clamped.
pub fn index_at(col: usize, row: usize) -> usize {
    row.min(ROWS - 1) * COLS + col.min(COLS - 1)
}

/// The palette entry equal to `tint`, if any.
pub fn index_of(tint: [u8; 3]) -> Option<usize> {
    PALETTE.iter().position(|c| *c == tint)
}

/// The palette entry nearest to `tint` (squared distance in RGB), for putting the cursor near a
/// colour that is not on the grid.
pub fn nearest(tint: [u8; 3]) -> usize {
    let dist = |c: &[u8; 3]| -> i32 { (0..3).map(|i| (i32::from(c[i]) - i32::from(tint[i])).pow(2)).sum() };
    PALETTE.iter().enumerate().min_by_key(|(_, c)| dist(c)).map_or(0, |(i, _)| i)
}

/// `tint` as `rrggbb` hex (lower case).
pub fn hex(tint: [u8; 3]) -> String {
    format!("{:02x}{:02x}{:02x}", tint[0], tint[1], tint[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_palette_fills_its_grid_with_distinct_bright_colours() {
        assert_eq!(PALETTE.len(), COLS * ROWS);
        let distinct: HashSet<[u8; 3]> = PALETTE.iter().copied().collect();
        assert_eq!(distinct.len(), PALETTE.len());
        for c in PALETTE {
            assert!(c.iter().copied().max().unwrap() >= 170, "{c:?} is too dark for a marker on a dark field");
        }
    }

    #[test]
    fn grid_positions_and_indices_are_inverse_and_clamped() {
        for i in 0..PALETTE.len() {
            let (c, r) = cell(i);
            assert_eq!(index_at(c, r), i);
            assert!(c < COLS && r < ROWS);
        }
        assert_eq!(index_at(99, 99), COLS * ROWS - 1);
        assert_eq!(index_at(0, 99), COLS);
        assert_eq!(index_at(99, 0), COLS - 1);
    }

    #[test]
    fn colours_are_found_exactly_or_by_nearness() {
        assert_eq!(index_of(PALETTE[5]), Some(5));
        assert_eq!(index_of([1, 2, 3]), None);
        assert_eq!(nearest(PALETTE[9]), 9);
        assert_eq!(nearest([236, 81, 79]), 0, "almost red");
        assert_eq!(nearest([0, 0, 0]), nearest([1, 1, 1]));
    }

    #[test]
    fn hex_is_six_lower_case_digits() {
        assert_eq!(hex([255, 136, 0]), "ff8800");
        assert_eq!(hex([0, 0, 0]), "000000");
        assert_eq!(crate::cli::parse_tint(&hex([1, 171, 255])), Ok([1, 171, 255]));
    }
}
