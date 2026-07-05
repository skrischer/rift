use alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui::Rgba;

const fn c(r: u8, g: u8, b: u8) -> Rgba {
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

pub const FOREGROUND: Rgba = c(205, 214, 244);
pub const BACKGROUND: Rgba = c(30, 30, 46);

const PALETTE: [Rgba; 16] = [
    c(69, 71, 90),    // black
    c(243, 139, 168), // red
    c(166, 227, 161), // green
    c(249, 226, 175), // yellow
    c(137, 180, 250), // blue
    c(245, 194, 231), // magenta
    c(148, 226, 213), // cyan
    c(186, 194, 222), // white
    c(88, 91, 112),   // bright black
    c(243, 139, 168), // bright red
    c(166, 227, 161), // bright green
    c(249, 226, 175), // bright yellow
    c(137, 180, 250), // bright blue
    c(245, 194, 231), // bright magenta
    c(148, 226, 213), // bright cyan
    c(205, 214, 244), // bright white
];

pub fn to_gpui_color(color: Color) -> Rgba {
    match color {
        Color::Named(named) => named_to_rgba(named),
        Color::Spec(rgb) => c(rgb.r, rgb.g, rgb.b),
        Color::Indexed(idx) => indexed_to_rgba(idx),
    }
}

fn named_to_rgba(named: NamedColor) -> Rgba {
    match named {
        NamedColor::Black => PALETTE[0],
        NamedColor::Red => PALETTE[1],
        NamedColor::Green => PALETTE[2],
        NamedColor::Yellow => PALETTE[3],
        NamedColor::Blue => PALETTE[4],
        NamedColor::Magenta => PALETTE[5],
        NamedColor::Cyan => PALETTE[6],
        NamedColor::White => PALETTE[7],
        NamedColor::BrightBlack => PALETTE[8],
        NamedColor::BrightRed => PALETTE[9],
        NamedColor::BrightGreen => PALETTE[10],
        NamedColor::BrightYellow => PALETTE[11],
        NamedColor::BrightBlue => PALETTE[12],
        NamedColor::BrightMagenta => PALETTE[13],
        NamedColor::BrightCyan => PALETTE[14],
        NamedColor::BrightWhite => PALETTE[15],
        NamedColor::Foreground | NamedColor::Cursor => FOREGROUND,
        _ => BACKGROUND,
    }
}

// xterm 6x6x6 color cube levels (not linear: 0, then 95 + 40 per step).
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn indexed_to_rgba(idx: u8) -> Rgba {
    if idx < 16 {
        return PALETTE[idx as usize];
    }
    if idx < 232 {
        let i = (idx - 16) as usize;
        let r = CUBE_LEVELS[i / 36];
        let g = CUBE_LEVELS[(i % 36) / 6];
        let b = CUBE_LEVELS[i % 6];
        return c(r, g, b);
    }
    let gray = 8 + (idx - 232) * 10;
    c(gray, gray, gray)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_gpui_color_cube_corners_xterm_rgb() {
        assert_eq!(to_gpui_color(Color::Indexed(16)), c(0, 0, 0));
        assert_eq!(to_gpui_color(Color::Indexed(21)), c(0, 0, 255));
        assert_eq!(to_gpui_color(Color::Indexed(196)), c(255, 0, 0));
        assert_eq!(to_gpui_color(Color::Indexed(231)), c(255, 255, 255));
    }

    #[test]
    fn test_to_gpui_color_cube_interior_nonlinear_levels() {
        // idx 110 -> cube coords (2, 3, 4) -> xterm levels (135, 175, 215)
        assert_eq!(to_gpui_color(Color::Indexed(110)), c(135, 175, 215));
        // idx 17 -> cube coords (0, 0, 1) -> first non-zero level is 95, not 51
        assert_eq!(to_gpui_color(Color::Indexed(17)), c(0, 0, 95));
    }

    #[test]
    fn test_to_gpui_color_palette_index_uses_palette() {
        assert_eq!(to_gpui_color(Color::Indexed(1)), PALETTE[1]);
    }

    #[test]
    fn test_to_gpui_color_grayscale_ramp_endpoints() {
        assert_eq!(to_gpui_color(Color::Indexed(232)), c(8, 8, 8));
        assert_eq!(to_gpui_color(Color::Indexed(255)), c(238, 238, 238));
    }
}
