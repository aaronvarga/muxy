//! Grid choices for the command center, independent of project layouts.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridLayout {
    #[default]
    Auto,
    OneByOne,
    TwoByOne,
    ThreeByOne,
    FourByOne,
    OneByTwo,
    OneByThree,
    TwoByTwo,
    ThreeByTwo,
    TwoByThree,
    FourByTwo,
    FiveByTwo,
    ThreeByThree,
    FourByThree,
    FourByFour,
    Custom([u8; 2]),
}

impl GridLayout {
    pub const ALL: [Self; 15] = [
        Self::Auto,
        Self::OneByOne,
        Self::TwoByOne,
        Self::ThreeByOne,
        Self::FourByOne,
        Self::OneByTwo,
        Self::OneByThree,
        Self::TwoByTwo,
        Self::ThreeByTwo,
        Self::TwoByThree,
        Self::FourByTwo,
        Self::FiveByTwo,
        Self::ThreeByThree,
        Self::FourByThree,
        Self::FourByFour,
    ];

    pub fn dimensions(self) -> Option<[u8; 2]> {
        Some(match self {
            Self::Auto => return None,
            Self::Custom([columns, rows]) => [columns.clamp(1, 10), rows.clamp(1, 8)],
            Self::OneByOne => [1, 1],
            Self::TwoByOne => [2, 1],
            Self::ThreeByOne => [3, 1],
            Self::FourByOne => [4, 1],
            Self::OneByTwo => [1, 2],
            Self::OneByThree => [1, 3],
            Self::TwoByTwo => [2, 2],
            Self::ThreeByTwo => [3, 2],
            Self::TwoByThree => [2, 3],
            Self::FourByTwo => [4, 2],
            Self::FiveByTwo => [5, 2],
            Self::ThreeByThree => [3, 3],
            Self::FourByThree => [4, 3],
            Self::FourByFour => [4, 4],
        })
    }

    pub fn label(self) -> String {
        match self {
            Self::Custom([columns, rows]) => {
                return format!("{} × {}", columns.clamp(1, 10), rows.clamp(1, 8));
            }
            Self::Auto => "Auto",
            Self::OneByOne => "1 × 1",
            Self::TwoByOne => "2 × 1",
            Self::ThreeByOne => "3 × 1",
            Self::FourByOne => "4 × 1",
            Self::OneByTwo => "1 × 2",
            Self::OneByThree => "1 × 3",
            Self::TwoByTwo => "2 × 2",
            Self::ThreeByTwo => "3 × 2",
            Self::TwoByThree => "2 × 3",
            Self::FourByTwo => "4 × 2",
            Self::FiveByTwo => "5 × 2",
            Self::ThreeByThree => "3 × 3",
            Self::FourByThree => "4 × 3",
            Self::FourByFour => "4 × 4",
        }
        .to_owned()
    }

    /// `width` and `height` are in unscaled points. Auto favors readable
    /// terminals; excess sessions occupy further scrollable rows.
    pub fn grid(self, count: usize, width: f32, height: f32) -> [u8; 2] {
        if let Some([columns, rows]) = self.dimensions() {
            let needed = count.div_ceil(usize::from(columns));
            return [columns, rows.max(u8::try_from(needed).unwrap_or(u8::MAX))];
        }
        let mut fallback = [1, 1];
        let mut best = None;
        let mut score = f32::INFINITY;
        for columns in 1..=4_u8 {
            for rows in 1..=4_u8 {
                let capacity = usize::from(columns * rows);
                if (columns > 1 && width / f32::from(columns) < 320.0)
                    || (rows > 1 && height / f32::from(rows) < 180.0)
                    || usize::from(columns) > count.max(1)
                {
                    continue;
                }
                if columns * rows > fallback[0] * fallback[1] {
                    fallback = [columns, rows];
                }
                if capacity < count.max(1) {
                    continue;
                }
                let empty = columns * rows - u8::try_from(count.max(1)).unwrap_or(u8::MAX);
                let aspect = width / f32::from(columns) / (height / f32::from(rows)).max(1.0);
                let candidate = (aspect - 2.0).abs() + f32::from(empty) * 0.5;
                if candidate < score {
                    best = Some([columns, rows]);
                    score = candidate;
                }
            }
        }
        best.unwrap_or(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grids_preserve_manual_choices_and_keep_large_or_narrow_dashboards_readable() {
        assert_eq!(GridLayout::Custom([0, 0]).grid(4, 1200.0, 600.0), [1, 4]);
        assert_eq!(GridLayout::Custom([4, 2]).grid(9, 1200.0, 600.0), [4, 3]);
        assert_eq!(GridLayout::Custom([255, 255]).dimensions(), Some([10, 8]));
        assert_eq!(GridLayout::TwoByTwo.grid(4, 1200.0, 600.0), [2, 2]);
        assert_eq!(GridLayout::TwoByTwo.grid(20, 1200.0, 600.0), [2, 10]);
        assert_eq!(GridLayout::FourByTwo.grid(9, 1600.0, 900.0), [4, 3]);
        assert_eq!(GridLayout::FourByTwo.grid(8, 1600.0, 900.0), [4, 2]);
        assert_eq!(GridLayout::FiveByTwo.grid(11, 1600.0, 900.0), [5, 3]);
        assert_eq!(
            GridLayout::FourByTwo.grid(usize::MAX, 1600.0, 900.0),
            [4, u8::MAX]
        );
        assert_eq!(GridLayout::Auto.grid(4, 1200.0, 600.0), [2, 2]);
        assert_eq!(GridLayout::Auto.grid(6, 1200.0, 600.0), [3, 2]);
        assert_eq!(GridLayout::Auto.grid(1000, 300.0, 400.0), [1, 2]);
        assert_eq!(GridLayout::Auto.grid(0, 0.0, 0.0), [1, 1]);
    }
}
