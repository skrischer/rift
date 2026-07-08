//! The command palette's registry (`docs/spec-command-palette.md`): a single,
//! curated list mapping a human display name (and, where bound, a keybinding
//! hint) to a dispatchable parameterless action. Seeded with rift's existing
//! editor/nav actions (`crate::editor`), the shell command actions Phase 16
//! defines (`crate::workspace`), and the Phase 17 theme commands (`crate`,
//! `docs/spec-theme-settings.md`, issue #367); argument-taking actions like
//! the terminal's `SelectWindow(usize)` are deliberately excluded.
//!
//! Curated, not auto-discovered (constitution: no premature abstraction) —
//! a new command is added here, one entry at a time, not by scattering
//! palette knowledge across the app.

use gpui::Action;

use crate::editor::{FindReferences, GoToDefinition, GoToLine, Save, ShowHover};
use crate::workspace::{
    FocusTerminal, NewSession, RefreshKeyTables, SwitchSession, ToggleExplorer, ToggleOutline,
    ToggleProblems, ToggleSourceControl, ZoomActivePanel,
};
use crate::{
    SelectCatppuccinMochaTheme, SelectDefaultDarkTheme, SelectDefaultLightTheme, ToggleThemeMode,
};

/// One command palette entry: a unique display name, an optional keybinding
/// hint to show alongside it, and a factory for the parameterless action it
/// dispatches.
pub struct Command {
    pub name: &'static str,
    pub keybinding_hint: Option<&'static str>,
    build: fn() -> Box<dyn Action>,
}

impl Command {
    const fn new(
        name: &'static str,
        keybinding_hint: Option<&'static str>,
        build: fn() -> Box<dyn Action>,
    ) -> Self {
        Self {
            name,
            keybinding_hint,
            build,
        }
    }

    /// Build a fresh instance of this command's dispatchable action.
    pub fn action(&self) -> Box<dyn Action> {
        (self.build)()
    }
}

/// The curated command registry: rift's existing editor/nav actions plus the
/// shell command actions this phase defines. Order is the palette's default
/// (unfiltered) list order.
pub const COMMANDS: &[Command] = &[
    Command::new("Save", Some("Ctrl+S"), || Box::new(Save)),
    Command::new("Go to Definition", Some("F12"), || Box::new(GoToDefinition)),
    Command::new("Show Hover", Some("Ctrl+K Ctrl+I"), || Box::new(ShowHover)),
    Command::new("Find References", Some("Shift+F12"), || {
        Box::new(FindReferences)
    }),
    Command::new("Go to Line", Some("Ctrl+G"), || Box::new(GoToLine)),
    Command::new("Toggle Explorer", None, || Box::new(ToggleExplorer)),
    Command::new("Toggle Outline", None, || Box::new(ToggleOutline)),
    Command::new("Toggle Problems", None, || Box::new(ToggleProblems)),
    Command::new("Toggle Source Control", None, || {
        Box::new(ToggleSourceControl)
    }),
    Command::new("Focus Terminal", None, || Box::new(FocusTerminal)),
    Command::new("Zoom Active Panel", None, || Box::new(ZoomActivePanel)),
    Command::new("Switch Session...", None, || Box::new(SwitchSession)),
    Command::new("New Session...", None, || Box::new(NewSession)),
    Command::new("Refresh tmux key tables", None, || {
        Box::new(RefreshKeyTables)
    }),
    Command::new("Toggle Light/Dark Theme", None, || {
        Box::new(ToggleThemeMode)
    }),
    Command::new("Select Theme: Default Light", None, || {
        Box::new(SelectDefaultLightTheme)
    }),
    Command::new("Select Theme: Default Dark", None, || {
        Box::new(SelectDefaultDarkTheme)
    }),
    Command::new("Select Theme: Catppuccin Mocha", None, || {
        Box::new(SelectCatppuccinMochaTheme)
    }),
];

/// Look up a command by its exact display name.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_display_names_are_unique() {
        for (i, a) in COMMANDS.iter().enumerate() {
            for b in &COMMANDS[i + 1..] {
                assert_ne!(a.name, b.name, "duplicate command display name: {}", a.name);
            }
        }
    }

    /// The curated set is exact — an argument-taking action (e.g. the
    /// terminal's `SelectWindow(usize)`) sneaking in would show up as an
    /// unexpected entry here.
    #[test]
    fn test_registry_is_seeded_with_the_curated_editor_shell_and_theme_commands() {
        let names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
        assert_eq!(
            names,
            vec![
                "Save",
                "Go to Definition",
                "Show Hover",
                "Find References",
                "Go to Line",
                "Toggle Explorer",
                "Toggle Outline",
                "Toggle Problems",
                "Toggle Source Control",
                "Focus Terminal",
                "Zoom Active Panel",
                "Switch Session...",
                "New Session...",
                "Refresh tmux key tables",
                "Toggle Light/Dark Theme",
                "Select Theme: Default Light",
                "Select Theme: Default Dark",
                "Select Theme: Catppuccin Mocha",
            ]
        );
    }

    /// Regression for #435: a bare `Shift+K` hover binding swallowed typing a
    /// capital 'K' into the buffer. The hint must match the non-typing chord
    /// bound in `main.rs`; drifting back to a plain typing key fails here.
    #[test]
    fn test_show_hover_hint_chord_binding_matches_non_typing_chord() {
        let hover = find("Show Hover").expect("Show Hover is registered");
        assert_eq!(hover.keybinding_hint, Some("Ctrl+K Ctrl+I"));
    }

    #[test]
    fn test_find_looks_up_a_command_by_display_name() {
        let save = find("Save").expect("Save is registered");
        assert!(save.action().partial_eq(&Save));
        assert_eq!(save.keybinding_hint, Some("Ctrl+S"));

        assert!(find("does not exist").is_none());
    }

    /// Go to Line (`docs/spec-v1-hardening.md`, #620): registered under the
    /// display name the palette renders, dispatching the action that opens
    /// the go-to-line dialog, with the hint matching the `Ctrl+G` binding in
    /// `main.rs`.
    #[test]
    fn test_go_to_line_is_registered_and_dispatches_the_expected_action() {
        let go_to_line = find("Go to Line").expect("Go to Line is registered");
        assert!(go_to_line.action().partial_eq(&GoToLine));
        assert_eq!(go_to_line.keybinding_hint, Some("Ctrl+G"));
    }

    /// Theme commands (issue #367): registered under the display names the
    /// palette renders, each dispatching the expected parameterless action —
    /// `set_theme`/`set_theme_mode` wiring is unit-tested directly in `lib.rs`.
    #[test]
    fn test_theme_commands_are_registered_and_dispatch_the_expected_actions() {
        let toggle = find("Toggle Light/Dark Theme").expect("theme toggle is registered");
        assert!(toggle.action().partial_eq(&ToggleThemeMode));

        let light = find("Select Theme: Default Light").expect("default light is registered");
        assert!(light.action().partial_eq(&SelectDefaultLightTheme));

        let dark = find("Select Theme: Default Dark").expect("default dark is registered");
        assert!(dark.action().partial_eq(&SelectDefaultDarkTheme));

        let mocha = find("Select Theme: Catppuccin Mocha").expect("catppuccin mocha is registered");
        assert!(mocha.action().partial_eq(&SelectCatppuccinMochaTheme));
    }

    /// Session-switcher commands (`docs/spec-session-switch.md`, issue #466):
    /// registered under the display names the palette renders, each
    /// dispatching the workspace-handled action that opens the switcher (with
    /// the new-session prompt active for "New Session...").
    #[test]
    fn test_session_commands_are_registered_and_dispatch_the_expected_actions() {
        let switch = find("Switch Session...").expect("switch session is registered");
        assert!(switch.action().partial_eq(&SwitchSession));

        let new = find("New Session...").expect("new session is registered");
        assert!(new.action().partial_eq(&NewSession));
    }

    /// "Dispatch shape": every entry builds a distinct, rift-namespaced
    /// action — checked via `gpui::Action::name()`, no live GPUI window
    /// needed since it is plain data on the action type.
    #[test]
    fn test_each_command_builds_a_rift_namespaced_action() {
        for command in COMMANDS {
            let action = command.action();
            assert!(
                action.name().starts_with("rift::"),
                "{} must dispatch a rift-namespace action, got {}",
                command.name,
                action.name()
            );
        }
    }
}
