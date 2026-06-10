//! Standalone component gallery: a Storybook-style dev window that renders
//! gpui-component widgets against rift's Catppuccin Mocha theme.
//!
//! This is the launchable shell only. Each entry's demo is a self-contained
//! `render_fn`; the comprehensive component coverage is filled in by follow-up
//! gallery issues. Gated behind the `gallery` cargo feature and launched via the
//! `gallery` binary (`just gallery`), so the shipping `rift` build is unaffected.

use gpui::{prelude::*, *};
use gpui_component::{
    input::{Input, InputEvent, InputState},
    resizable::{h_resizable, resizable_panel},
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    v_flex, ActiveTheme as _, Root,
};
use gpui_component_assets::Assets;
use tracing_subscriber::EnvFilter;

/// Renders a single component's demo. Takes the same `(window, cx)` a `Render`
/// impl receives so demos can build stateful gpui-component widgets.
type RenderFn = fn(&mut Window, &mut App) -> AnyElement;

/// One gallery entry: a display name, a one-line description, and its demo
/// renderer.
#[derive(Clone, Copy)]
pub struct ComponentEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub render: RenderFn,
}

/// The flat list of component entries shown in the sidebar. Follow-up issues
/// extend this with the real component demos; the skeleton ships one placeholder.
pub fn registry() -> Vec<ComponentEntry> {
    vec![ComponentEntry {
        name: "Placeholder",
        description: "Skeleton entry. Component demos land in follow-up gallery issues.",
        render: render_placeholder,
    }]
}

fn render_placeholder(_window: &mut Window, cx: &mut App) -> AnyElement {
    v_flex()
        .gap_2()
        .child(div().text_lg().child("Placeholder"))
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child("Component demos are added in follow-up gallery issues."),
        )
        .into_any_element()
}

/// The gallery view: a searchable sidebar of [`ComponentEntry`]s beside a content
/// pane rendering the selected entry's demo. Modeled on upstream
/// `gpui-component-story`'s `gallery.rs`, minus the dockable `StoryContainer`
/// machinery rift's gallery does not need.
pub struct Gallery {
    entries: Vec<ComponentEntry>,
    /// Index into `entries` of the selected entry (selection survives filtering).
    active_index: Option<usize>,
    search_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl Gallery {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search..."));
        let _subscriptions = vec![cx.subscribe(&search_input, |_, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        })];
        let entries = registry();
        let active_index = (!entries.is_empty()).then_some(0);
        Self {
            entries,
            active_index,
            search_input,
            _subscriptions,
        }
    }
}

impl Render for Gallery {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search_input.read(cx).value().trim().to_lowercase();

        // Owned (full-list index, entry) pairs so the menu builder borrows neither
        // `self.entries` nor `query` while it also uses `cx`.
        let filtered: Vec<(usize, ComponentEntry)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.name.to_lowercase().contains(&query)
                    || e.description.to_lowercase().contains(&query)
            })
            .map(|(i, e)| (i, *e))
            .collect();

        let active = self.active_index.and_then(|i| self.entries.get(i).copied());
        let (title, description) = active.map(|e| (e.name, e.description)).unwrap_or_default();
        // The closure is an immediate `FnOnce`, so it reborrows `window`/`cx` —
        // both stay usable while the rest of the tree is built below.
        let content = active.map(|e| (e.render)(window, cx));

        h_resizable("gallery-container")
            .child(
                resizable_panel()
                    .size(px(255.))
                    .size_range(px(200.)..px(360.))
                    .child(
                        Sidebar::new("gallery-sidebar")
                            .w(relative(1.))
                            .border_0()
                            .header(
                                div().w_full().px_2().child(
                                    Input::new(&self.search_input)
                                        .appearance(false)
                                        .cleanable(true),
                                ),
                            )
                            .child(SidebarGroup::new("Components").child(
                                SidebarMenu::new().children(filtered.into_iter().map(
                                    |(full_ix, entry)| {
                                        SidebarMenuItem::new(entry.name)
                                            .active(self.active_index == Some(full_ix))
                                            .on_click(cx.listener(
                                                move |this, _: &ClickEvent, _, cx| {
                                                    this.active_index = Some(full_ix);
                                                    cx.notify();
                                                },
                                            ))
                                    },
                                )),
                            )),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .overflow_x_hidden()
                    .child(
                        v_flex()
                            .p_4()
                            .gap_1()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(div().text_xl().child(title))
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(description),
                            ),
                    )
                    .child(
                        div()
                            .id("gallery-content")
                            .flex_1()
                            .overflow_y_scroll()
                            .p_4()
                            .when_some(content, |this, content| this.child(content)),
                    )
                    .into_any_element(),
            )
    }
}

/// Launch the gallery window. Mirrors `main.rs`'s window setup and runs the same
/// [`crate::apply_theme`] path so the gallery renders in rift's palette, then loads
/// gpui-component icon assets via [`Assets`].
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .init();

    Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(Assets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            crate::apply_theme(cx);
            let bounds = Bounds::centered(None, size(px(1400.0), px(900.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Maximized(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    let gallery = cx.new(|cx| Gallery::new(window, cx));
                    cx.new(|cx| Root::new(gallery, window, cx))
                },
            )
            .expect("failed to open gallery window");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    // Import only `registry`, not `super::*`: the module glob-imports `gpui::*`,
    // which brings gpui's `test` attribute macro into scope and shadows the
    // built-in `#[test]`, blowing the macro recursion limit.
    use super::registry;
    use std::collections::HashSet;

    #[test]
    fn test_registry_is_non_empty() {
        assert!(
            !registry().is_empty(),
            "gallery registry must have at least one entry"
        );
    }

    #[test]
    fn test_registry_names_unique() {
        let entries = registry();
        let unique: HashSet<&str> = entries.iter().map(|e| e.name).collect();
        assert_eq!(
            unique.len(),
            entries.len(),
            "gallery entry names must be unique"
        );
    }
}
