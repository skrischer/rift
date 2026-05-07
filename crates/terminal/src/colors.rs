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
pub const SURFACE0: Rgba = c(49, 50, 68);
pub const SURFACE1: Rgba = c(69, 71, 90);
pub const SUBTEXT0: Rgba = c(166, 173, 200);

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

fn indexed_to_rgba(idx: u8) -> Rgba {
    if idx < 16 {
        return PALETTE[idx as usize];
    }
    if idx < 232 {
        let i = idx - 16;
        let r = (i / 36) * 51;
        let g = ((i % 36) / 6) * 51;
        let b = (i % 6) * 51;
        return c(r, g, b);
    }
    let gray = 8 + (idx - 232) * 10;
    c(gray, gray, gray)
}
