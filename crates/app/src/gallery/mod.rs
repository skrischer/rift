//! Standalone component gallery: a Storybook-style dev window that renders
//! gpui-component widgets against rift's Catppuccin Mocha theme.
//!
//! Entries live in a flat [`registry`]; each is a self-contained demo (a
//! stateless [`Demo::Element`] or a stateful [`Demo::View`]). Gated behind the
//! `gallery` cargo feature and launched via the `gallery` binary
//! (`just gallery`), so the shipping `rift` build is unaffected.

use gpui::{prelude::*, *};
use gpui_component::{
    input::{Input, InputEvent, InputState},
    resizable::{h_resizable, resizable_panel},
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    v_flex, ActiveTheme as _, Root,
};
use gpui_component_assets::Assets;
use tracing_subscriber::EnvFilter;

mod demos;

/// How a gallery entry produces its content. Stateless demos are a plain `fn`
/// rebuilt every frame; stateful demos build a view entity once (cached by the
/// [`Gallery`]) so their widget state (`InputState`, `SliderState`, …) survives
/// across frames — a function pointer alone cannot hold that state.
#[derive(Clone, Copy)]
pub enum Demo {
    /// Stateless: rebuilt from scratch on every frame.
    Element(fn(&mut Window, &mut App) -> AnyElement),
    /// Stateful: built once into a view, then rendered as a cloneable handle.
    View(fn(&mut Window, &mut App) -> AnyView),
}

/// One gallery entry: a display name, a one-line description, and its demo.
#[derive(Clone, Copy)]
pub struct ComponentEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub demo: Demo,
}

/// The flat list of component entries shown in the sidebar. Part 1 (#124) covers
/// the Theme-tokens reference plus the form/input and feedback widgets; later
/// gallery issues append more entries.
pub fn registry() -> Vec<ComponentEntry> {
    use Demo::{Element, View};
    vec![
        ComponentEntry {
            name: "Theme Tokens",
            description: "Active theme color swatches with their token names.",
            demo: Element(demos::render_theme_tokens),
        },
        ComponentEntry {
            name: "Button",
            description: "Variants, icon, loading and disabled states.",
            demo: Element(demos::render_button),
        },
        ComponentEntry {
            name: "Dropdown Button",
            description: "A button paired with a popup menu of actions.",
            demo: Element(demos::render_dropdown_button),
        },
        ComponentEntry {
            name: "Input",
            description: "Text, password and number inputs.",
            demo: View(demos::build_input),
        },
        ComponentEntry {
            name: "OTP Input",
            description: "Grouped one-time-code inputs.",
            demo: View(demos::build_otp),
        },
        ComponentEntry {
            name: "Textarea",
            description: "Multi-line text input.",
            demo: View(demos::build_textarea),
        },
        ComponentEntry {
            name: "Label",
            description: "Labels with secondary and muted text.",
            demo: Element(demos::render_label),
        },
        ComponentEntry {
            name: "Checkbox",
            description: "Checked, unchecked and disabled states.",
            demo: Element(demos::render_checkbox),
        },
        ComponentEntry {
            name: "Radio",
            description: "Single radios and a radio group.",
            demo: Element(demos::render_radio),
        },
        ComponentEntry {
            name: "Switch",
            description: "On, off and disabled toggles.",
            demo: Element(demos::render_switch),
        },
        ComponentEntry {
            name: "Select",
            description: "A searchable single-select dropdown.",
            demo: View(demos::build_select),
        },
        ComponentEntry {
            name: "Combobox",
            description: "A searchable combobox.",
            demo: View(demos::build_combobox),
        },
        ComponentEntry {
            name: "Form",
            description: "A vertical form with labeled fields.",
            demo: View(demos::build_form),
        },
        ComponentEntry {
            name: "Slider",
            description: "Single-value and range sliders.",
            demo: View(demos::build_slider),
        },
        ComponentEntry {
            name: "Progress",
            description: "Determinate progress bars.",
            demo: Element(demos::render_progress),
        },
        ComponentEntry {
            name: "Spinner",
            description: "Indeterminate loading spinners.",
            demo: Element(demos::render_spinner),
        },
        ComponentEntry {
            name: "Skeleton",
            description: "Loading placeholders.",
            demo: Element(demos::render_skeleton),
        },
        ComponentEntry {
            name: "Badge",
            description: "Count and dot badges on a child.",
            demo: Element(demos::render_badge),
        },
        ComponentEntry {
            name: "Tag",
            description: "Colored and outline tags.",
            demo: Element(demos::render_tag),
        },
        ComponentEntry {
            name: "Alert",
            description: "Info, success, warning and error alerts.",
            demo: Element(demos::render_alert),
        },
        ComponentEntry {
            name: "Notification",
            description: "Push notifications to the window.",
            demo: Element(demos::render_notification),
        },
        ComponentEntry {
            name: "Tooltip",
            description: "Tooltips on interactive elements.",
            demo: Element(demos::render_tooltip),
        },
        ComponentEntry {
            name: "Rating",
            description: "Star rating display.",
            demo: Element(demos::render_rating),
        },
    ]
}

/// The gallery view: a searchable sidebar of [`ComponentEntry`]s beside a content
/// pane rendering the selected entry's demo. Modeled on upstream
/// `gpui-component-story`'s `gallery.rs`, minus the dockable `StoryContainer`
/// machinery rift's gallery does not need.
pub struct Gallery {
    entries: Vec<ComponentEntry>,
    /// Lazily-built view handles for [`Demo::View`] entries, parallel to
    /// `entries`. Each stateful demo is built once on first selection and reused.
    views: Vec<Option<AnyView>>,
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
        let views = vec![None; entries.len()];
        Self {
            entries,
            views,
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

        let active_ix = self.active_index;
        let (title, description) = active_ix
            .and_then(|i| self.entries.get(i))
            .map(|e| (e.name, e.description))
            .unwrap_or_default();

        // Resolve the active demo's content. Reading `demo` copies it (both
        // variants are `Copy`), so no borrow of `self.entries` lingers while a
        // `View` demo borrows `self.views` to build and cache its entity once.
        let content: Option<AnyElement> = active_ix.map(|i| match self.entries[i].demo {
            // `demo` is `Copy`, so this read leaves no borrow on `self.entries`.
            Demo::Element(render) => render(window, cx),
            Demo::View(build) => {
                if self.views[i].is_none() {
                    self.views[i] = Some(build(window, cx));
                }
                self.views[i]
                    .clone()
                    .map_or_else(|| div().into_any_element(), |v| v.into_any_element())
            }
        });

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
