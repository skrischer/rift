use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermDamage};
use gpui::Rgba;

#[derive(Clone, Debug)]
pub struct CellRenderInfo {
    pub col: usize,
    pub row: usize,
    pub ch: char,
    pub fg: Rgba,
    pub bg: Rgba,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub wide: bool,
    pub wide_spacer: bool,
    pub selected: bool,
}

#[derive(Clone, Debug)]
pub enum DamageSnapshot {
    Full,
    Partial(Vec<DirtySpan>),
}

#[derive(Clone, Debug)]
pub struct DirtySpan {
    pub row: usize,
    pub left: usize,
    pub right: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridSelection {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

impl GridSelection {
    pub fn normalize(&self) -> (usize, usize, usize, usize) {
        if self.start_row < self.end_row
            || (self.start_row == self.end_row && self.start_col <= self.end_col)
        {
            (self.start_row, self.start_col, self.end_row, self.end_col)
        } else {
            (self.end_row, self.end_col, self.start_row, self.start_col)
        }
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        let (sr, sc, er, ec) = self.normalize();

        if row < sr || row > er {
            return false;
        }

        if sr == er {
            return col >= sc && col <= ec;
        }

        if row == sr {
            return col >= sc;
        }

        if row == er {
            return col <= ec;
        }

        true
    }
}

pub fn take_damage<T: EventListener>(term: &mut Term<T>) -> DamageSnapshot {
    let snapshot = match term.damage() {
        TermDamage::Full => DamageSnapshot::Full,
        TermDamage::Partial(iter) => {
            let spans = iter
                .map(|bounds| DirtySpan {
                    row: bounds.line,
                    left: bounds.left,
                    right: bounds.right,
                })
                .collect();
            DamageSnapshot::Partial(spans)
        }
    };
    term.reset_damage();
    snapshot
}

pub fn extract_row_cells<T: EventListener>(
    term: &Term<T>,
    row: usize,
    selection: Option<&GridSelection>,
) -> Vec<CellRenderInfo> {
    let grid = term.grid();
    let columns = grid.columns();
    let mut cells = Vec::with_capacity(columns);

    for col in 0..columns {
        let cell = &grid[Line(row as i32)][Column(col)];

        let flags = cell.flags;
        let inverse = flags.contains(Flags::INVERSE);

        let raw_fg = crate::colors::to_gpui_color(cell.fg);
        let raw_bg = crate::colors::to_gpui_color(cell.bg);

        let (fg, bg) = if inverse {
            (raw_bg, raw_fg)
        } else {
            (raw_fg, raw_bg)
        };

        let ch = if cell.c == '\0' { ' ' } else { cell.c };

        let selected = selection.is_some_and(|sel| sel.contains(row, col));

        cells.push(CellRenderInfo {
            col,
            row,
            ch,
            fg,
            bg,
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
            strikethrough: flags.contains(Flags::STRIKEOUT),
            wide: flags.contains(Flags::WIDE_CHAR),
            wide_spacer: flags.contains(Flags::WIDE_CHAR_SPACER),
            selected,
        });
    }

    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions as DimTrait;
    use alacritty_terminal::term::Config;

    struct TestSize {
        cols: usize,
        rows: usize,
    }

    impl DimTrait for TestSize {
        fn total_lines(&self) -> usize {
            self.rows
        }

        fn screen_lines(&self) -> usize {
            self.rows
        }

        fn columns(&self) -> usize {
            self.cols
        }
    }

    fn make_term(cols: usize, rows: usize) -> Term<VoidListener> {
        let size = TestSize { cols, rows };
        Term::new(Config::default(), &size, VoidListener)
    }

    #[test]
    fn test_grid_selection_contains_single_row() {
        let sel = GridSelection {
            start_row: 5,
            start_col: 3,
            end_row: 5,
            end_col: 10,
        };

        assert!(!sel.contains(5, 2));
        assert!(sel.contains(5, 3));
        assert!(sel.contains(5, 7));
        assert!(sel.contains(5, 10));
        assert!(!sel.contains(5, 11));
        assert!(!sel.contains(4, 5));
        assert!(!sel.contains(6, 5));
    }

    #[test]
    fn test_grid_selection_contains_multi_row() {
        let sel = GridSelection {
            start_row: 2,
            start_col: 5,
            end_row: 4,
            end_col: 8,
        };

        assert!(!sel.contains(2, 4));
        assert!(sel.contains(2, 5));
        assert!(sel.contains(2, 79));

        assert!(sel.contains(3, 0));
        assert!(sel.contains(3, 40));
        assert!(sel.contains(3, 79));

        assert!(sel.contains(4, 0));
        assert!(sel.contains(4, 8));
        assert!(!sel.contains(4, 9));

        assert!(!sel.contains(1, 5));
        assert!(!sel.contains(5, 0));
    }

    #[test]
    fn test_grid_selection_contains_backward() {
        let sel = GridSelection {
            start_row: 4,
            start_col: 8,
            end_row: 2,
            end_col: 5,
        };

        assert!(!sel.contains(2, 4));
        assert!(sel.contains(2, 5));
        assert!(sel.contains(3, 0));
        assert!(sel.contains(4, 8));
        assert!(!sel.contains(4, 9));
    }

    #[test]
    fn test_grid_selection_normalize() {
        let forward = GridSelection {
            start_row: 2,
            start_col: 5,
            end_row: 4,
            end_col: 8,
        };
        assert_eq!(forward.normalize(), (2, 5, 4, 8));

        let backward = GridSelection {
            start_row: 4,
            start_col: 8,
            end_row: 2,
            end_col: 5,
        };
        assert_eq!(backward.normalize(), (2, 5, 4, 8));

        let same_row = GridSelection {
            start_row: 3,
            start_col: 10,
            end_row: 3,
            end_col: 2,
        };
        assert_eq!(same_row.normalize(), (3, 2, 3, 10));
    }

    #[test]
    fn test_damage_snapshot_from_full() {
        let mut term = make_term(80, 24);
        let snapshot = take_damage(&mut term);
        assert!(matches!(snapshot, DamageSnapshot::Full));
    }

    #[test]
    fn test_extract_row_cells_default_grid() {
        let term = make_term(80, 24);
        let cells = extract_row_cells(&term, 0, None);

        assert_eq!(cells.len(), 80);
        assert_eq!(cells[0].col, 0);
        assert_eq!(cells[0].row, 0);
        assert_eq!(cells[0].ch, ' ');
        assert!(!cells[0].bold);
        assert!(!cells[0].selected);
    }

    #[test]
    fn test_extract_row_cells_with_selection() {
        let term = make_term(80, 24);
        let sel = GridSelection {
            start_row: 0,
            start_col: 5,
            end_row: 0,
            end_col: 10,
        };
        let cells = extract_row_cells(&term, 0, Some(&sel));

        assert!(!cells[4].selected);
        assert!(cells[5].selected);
        assert!(cells[10].selected);
        assert!(!cells[11].selected);
    }

    #[test]
    fn test_damage_after_reset_is_partial() {
        let mut term = make_term(80, 24);
        take_damage(&mut term);

        let snapshot = take_damage(&mut term);
        assert!(matches!(snapshot, DamageSnapshot::Partial(_)));
    }
}
