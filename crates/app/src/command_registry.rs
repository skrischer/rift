//! The command palette's registry (`docs/spec-command-palette.md`): a single,
//! curated list mapping a human display name (and, where bound, a keybinding
//! hint) to a dispatchable parameterless action. Seeded with rift's existing
//! editor/nav actions (`crate::editor`) and the shell command actions Phase 16
//! defines (`crate::workspace`); argument-taking actions like the terminal's
//! `SelectWindow(usize)` are deliberately excluded.
//!
//! Curated, not auto-discovered (constitution: no premature abstraction) —
//! a new command is added here, one entry at a time, not by scattering
//! palette knowledge across the app.

use gpui::Action;

use crate::editor::{FindReferences, GoToDefinition, Save, ShowHover};
use crate::workspace::{
    FocusTerminal, ToggleExplorer, ToggleProblems, ToggleSourceControl, ZoomActivePanel,
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
    Command::new("Show Hover", Some("Shift+K"), || Box::new(ShowHover)),
    Command::new("Find References", Some("Shift+F12"), || {
        Box::new(FindReferences)
    }),
    Command::new("Toggle Explorer", None, || Box::new(ToggleExplorer)),
    Command::new("Toggle Problems", None, || Box::new(ToggleProblems)),
    Command::new("Toggle Source Control", None, || {
        Box::new(ToggleSourceControl)
    }),
    Command::new("Focus Terminal", None, || Box::new(FocusTerminal)),
    Command::new("Zoom Active Panel", None, || Box::new(ZoomActivePanel)),
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
    fn test_registry_is_seeded_with_the_curated_editor_and_shell_commands() {
        let names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
        assert_eq!(
            names,
            vec![
                "Save",
                "Go to Definition",
                "Show Hover",
                "Find References",
                "Toggle Explorer",
                "Toggle Problems",
                "Toggle Source Control",
                "Focus Terminal",
                "Zoom Active Panel",
            ]
        );
    }

    #[test]
    fn test_find_looks_up_a_command_by_display_name() {
        let save = find("Save").expect("Save is registered");
        assert!(save.action().partial_eq(&Save));
        assert_eq!(save.keybinding_hint, Some("Ctrl+S"));

        assert!(find("does not exist").is_none());
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
