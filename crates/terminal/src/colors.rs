use alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui::{Hsla, Rgba};
use gpui_component::{Colorize, ThemeColor};

/// Fraction of `border` kept when blending it toward `foreground` for the
/// bright-black slot, so slot 8 reads as a dim grey on every theme instead of
/// vanishing into the background (matches today's Catppuccin surface2).
const BRIGHT_BLACK_MIX: f32 = 0.85;

/// A terminal color palette resolved from the active theme. The 16 named /
/// `Indexed(< 16)` ANSI slots plus the default foreground/background map to
/// `gpui_component` theme tokens (`docs/spec-settings-theme.md`); the xterm
/// 6x6x6 color cube and grayscale ramp (`Indexed(>= 16)`) stay exact xterm RGB
/// — a terminal standard applications rely on, not a theme palette. Built once
/// per render from `cx.theme()`, so switching the theme restyles the grid live.
pub struct TerminalPalette {
    ansi: [Hsla; 16],
    foreground: Hsla,
    background: Hsla,
}

impl TerminalPalette {
    /// Build the palette from a theme's tokens. The neutral slots (black, white,
    /// bright-black, bright-white) map to structural tokens so every theme yields
    /// a coherent 16-color set; magenta intentionally becomes the theme's mauve.
    pub fn from_theme(theme: &ThemeColor) -> Self {
        let bright_black = theme.border.mix(theme.foreground, BRIGHT_BLACK_MIX);
        Self {
            ansi: [
                theme.border,           // 0 black
                theme.red,              // 1 red
                theme.green,            // 2 green
                theme.yellow,           // 3 yellow
                theme.blue,             // 4 blue
                theme.magenta,          // 5 magenta
                theme.cyan,             // 6 cyan
                theme.muted_foreground, // 7 white
                bright_black,           // 8 bright black
                theme.red_light,        // 9 bright red
                theme.green_light,      // 10 bright green
                theme.yellow_light,     // 11 bright yellow
                theme.blue_light,       // 12 bright blue
                theme.magenta_light,    // 13 bright magenta
                theme.cyan_light,       // 14 bright cyan
                theme.foreground,       // 15 bright white
            ],
            foreground: theme.foreground,
            background: theme.background,
        }
    }

    /// The default cell foreground (theme `foreground`).
    pub fn foreground(&self) -> Hsla {
        self.foreground
    }

    /// The default cell background (theme `background`).
    pub fn background(&self) -> Hsla {
        self.background
    }

    /// Resolve an alacritty cell color to a concrete `Hsla`.
    pub fn resolve(&self, color: Color) -> Hsla {
        match color {
            Color::Named(named) => self.named(named),
            Color::Spec(rgb) => rgb_to_hsla(rgb.r, rgb.g, rgb.b),
            Color::Indexed(idx) => self.indexed(idx),
        }
    }

    fn named(&self, named: NamedColor) -> Hsla {
        match named {
            NamedColor::Black => self.ansi[0],
            NamedColor::Red => self.ansi[1],
            NamedColor::Green => self.ansi[2],
            NamedColor::Yellow => self.ansi[3],
            NamedColor::Blue => self.ansi[4],
            NamedColor::Magenta => self.ansi[5],
            NamedColor::Cyan => self.ansi[6],
            NamedColor::White => self.ansi[7],
            NamedColor::BrightBlack => self.ansi[8],
            NamedColor::BrightRed => self.ansi[9],
            NamedColor::BrightGreen => self.ansi[10],
            NamedColor::BrightYellow => self.ansi[11],
            NamedColor::BrightBlue => self.ansi[12],
            NamedColor::BrightMagenta => self.ansi[13],
            NamedColor::BrightCyan => self.ansi[14],
            NamedColor::BrightWhite => self.ansi[15],
            NamedColor::Foreground | NamedColor::Cursor => self.foreground,
            _ => self.background,
        }
    }

    fn indexed(&self, idx: u8) -> Hsla {
        if idx < 16 {
            return self.ansi[idx as usize];
        }
        if idx < 232 {
            let i = (idx - 16) as usize;
            let r = CUBE_LEVELS[i / 36];
            let g = CUBE_LEVELS[(i % 36) / 6];
            let b = CUBE_LEVELS[i % 6];
            return rgb_to_hsla(r, g, b);
        }
        let gray = 8 + (idx - 232) * 10;
        rgb_to_hsla(gray, gray, gray)
    }
}

// xterm 6x6x6 color cube levels (not linear: 0, then 95 + 40 per step).
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn rgb_to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    Hsla::from(Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> TerminalPalette {
        TerminalPalette::from_theme(&ThemeColor::default())
    }

    #[test]
    fn test_resolve_cube_corners_stay_exact_xterm_rgb() {
        let p = palette();
        assert_eq!(p.resolve(Color::Indexed(16)), rgb_to_hsla(0, 0, 0));
        assert_eq!(p.resolve(Color::Indexed(21)), rgb_to_hsla(0, 0, 255));
        assert_eq!(p.resolve(Color::Indexed(196)), rgb_to_hsla(255, 0, 0));
        assert_eq!(p.resolve(Color::Indexed(231)), rgb_to_hsla(255, 255, 255));
    }

    #[test]
    fn test_resolve_cube_interior_uses_nonlinear_levels() {
        let p = palette();
        // idx 110 -> cube coords (2, 3, 4) -> xterm levels (135, 175, 215)
        assert_eq!(p.resolve(Color::Indexed(110)), rgb_to_hsla(135, 175, 215));
        // idx 17 -> cube coords (0, 0, 1) -> first non-zero level is 95, not 51
        assert_eq!(p.resolve(Color::Indexed(17)), rgb_to_hsla(0, 0, 95));
    }

    #[test]
    fn test_resolve_grayscale_ramp_endpoints_stay_exact_xterm_rgb() {
        let p = palette();
        assert_eq!(p.resolve(Color::Indexed(232)), rgb_to_hsla(8, 8, 8));
        assert_eq!(p.resolve(Color::Indexed(255)), rgb_to_hsla(238, 238, 238));
    }

    #[test]
    fn test_resolve_named_slots_map_to_theme_tokens() {
        let theme = ThemeColor::default();
        let p = TerminalPalette::from_theme(&theme);
        assert_eq!(p.resolve(Color::Named(NamedColor::Black)), theme.border);
        assert_eq!(p.resolve(Color::Named(NamedColor::Red)), theme.red);
        assert_eq!(p.resolve(Color::Named(NamedColor::Green)), theme.green);
        assert_eq!(p.resolve(Color::Named(NamedColor::Yellow)), theme.yellow);
        assert_eq!(p.resolve(Color::Named(NamedColor::Blue)), theme.blue);
        assert_eq!(p.resolve(Color::Named(NamedColor::Magenta)), theme.magenta);
        assert_eq!(p.resolve(Color::Named(NamedColor::Cyan)), theme.cyan);
        assert_eq!(
            p.resolve(Color::Named(NamedColor::White)),
            theme.muted_foreground
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::BrightBlack)),
            theme.border.mix(theme.foreground, BRIGHT_BLACK_MIX)
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::BrightRed)),
            theme.red_light
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::BrightGreen)),
            theme.green_light
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::BrightYellow)),
            theme.yellow_light
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::BrightBlue)),
            theme.blue_light
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::BrightMagenta)),
            theme.magenta_light
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::BrightCyan)),
            theme.cyan_light
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::BrightWhite)),
            theme.foreground
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Foreground)),
            theme.foreground
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Cursor)),
            theme.foreground
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Background)),
            theme.background
        );
        assert_eq!(p.foreground(), theme.foreground);
        assert_eq!(p.background(), theme.background);
    }

    #[test]
    fn test_resolve_indexed_below_16_matches_named_slot() {
        let theme = ThemeColor::default();
        let p = TerminalPalette::from_theme(&theme);
        assert_eq!(p.resolve(Color::Indexed(1)), theme.red);
        assert_eq!(
            p.resolve(Color::Indexed(8)),
            theme.border.mix(theme.foreground, BRIGHT_BLACK_MIX)
        );
    }
}
