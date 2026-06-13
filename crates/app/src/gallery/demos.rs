//! Component demos for the gallery. Part 1 (#124): the Theme-tokens reference
//! plus the form/input and feedback components. Part 2 (#125): the layout,
//! navigation, overlay and picker components.
//!
//! Each entry is either a stateless [`super::Demo::Element`] (rebuilt every
//! frame from a plain `fn`) or a stateful [`super::Demo::View`] backed by a small
//! view struct that owns the widget state entities (`InputState`, `SliderState`,
//! …) so they survive across frames. The flat registry in `mod.rs` points at the
//! `render_*` / `build_*` functions here. Demos use inline static data only — no
//! event handling beyond what a button needs to push a notification.

use gpui::{prelude::*, *};
use gpui_component::{
    accordion::Accordion,
    alert::Alert,
    avatar::Avatar,
    badge::Badge,
    breadcrumb::{Breadcrumb, BreadcrumbItem},
    button::{Button, ButtonVariants, DropdownButton},
    calendar::{Calendar, CalendarState},
    chart::{BarChart, LineChart},
    checkbox::Checkbox,
    clipboard::Clipboard,
    collapsible::Collapsible,
    color_picker::{ColorPicker, ColorPickerState},
    combobox::{Combobox, ComboboxState},
    date_picker::{DatePicker, DatePickerState},
    description_list::{DescriptionItem, DescriptionList},
    form::{field, v_form},
    group_box::{GroupBox, GroupBoxVariants as _},
    h_flex,
    hover_card::HoverCard,
    input::{Input, InputState, NumberInput, OtpInput, OtpState, TabSize},
    kbd::Kbd,
    label::Label,
    link::Link,
    list::{List, ListDelegate, ListItem, ListState},
    menu::{ContextMenuExt as _, DropdownMenu as _},
    notification::NotificationType,
    pagination::Pagination,
    popover::Popover,
    progress::Progress,
    radio::{Radio, RadioGroup},
    rating::Rating,
    resizable::{h_resizable, resizable_panel},
    scroll::ScrollableElement as _,
    searchable_list::SearchableVec,
    select::{Select, SelectState},
    separator::Separator,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    skeleton::Skeleton,
    slider::{Slider, SliderState},
    spinner::Spinner,
    stepper::{Stepper, StepperItem},
    switch::Switch,
    tab::{Tab, TabBar},
    table::{
        Column, DataTable, Table, TableBody, TableCell, TableDelegate, TableHead, TableHeader,
        TableRow, TableState,
    },
    tag::Tag,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _,
    WindowExt as _,
};

// Demo-only no-op actions for the dropdown-button menu. Unhandled at runtime,
// which is fine for a gallery preview.
gpui::actions!(rift_gallery, [DemoCopy, DemoPaste, DemoDelete]);

// ---------------------------------------------------------------------------
// Theme tokens
// ---------------------------------------------------------------------------

pub(super) fn render_theme_tokens(_window: &mut Window, cx: &mut App) -> AnyElement {
    let t = cx.theme();
    // Go through `t.colors` explicitly: `Theme` derefs to `ThemeColor` but also
    // has its own non-color fields (`list: ListSettings`, `sheet`, …) that shadow
    // same-named color tokens, so `t.list` would not be an `Hsla`.
    macro_rules! sw {
        ($field:ident) => {
            swatch_cell(stringify!($field), t.colors.$field, t.border)
        };
    }

    let cells: Vec<AnyElement> = vec![
        sw!(accent),
        sw!(accent_foreground),
        sw!(accordion),
        sw!(accordion_hover),
        sw!(background),
        sw!(border),
        sw!(button_primary),
        sw!(button_primary_active),
        sw!(button_primary_foreground),
        sw!(button_primary_hover),
        sw!(group_box),
        sw!(group_box_foreground),
        sw!(caret),
        sw!(chart_1),
        sw!(chart_2),
        sw!(chart_3),
        sw!(chart_4),
        sw!(chart_5),
        sw!(chart_bullish),
        sw!(chart_bearish),
        sw!(danger),
        sw!(danger_active),
        sw!(danger_foreground),
        sw!(danger_hover),
        sw!(description_list_label),
        sw!(description_list_label_foreground),
        sw!(drag_border),
        sw!(drop_target),
        sw!(foreground),
        sw!(info),
        sw!(info_active),
        sw!(info_foreground),
        sw!(info_hover),
        sw!(input),
        sw!(link),
        sw!(link_active),
        sw!(link_hover),
        sw!(list),
        sw!(list_active),
        sw!(list_active_border),
        sw!(list_even),
        sw!(list_head),
        sw!(list_hover),
        sw!(muted),
        sw!(muted_foreground),
        sw!(popover),
        sw!(popover_foreground),
        sw!(primary),
        sw!(primary_active),
        sw!(primary_foreground),
        sw!(primary_hover),
        sw!(progress_bar),
        sw!(ring),
        sw!(scrollbar),
        sw!(scrollbar_thumb),
        sw!(scrollbar_thumb_hover),
        sw!(secondary),
        sw!(secondary_active),
        sw!(secondary_foreground),
        sw!(secondary_hover),
        sw!(selection),
        sw!(sidebar),
        sw!(sidebar_accent),
        sw!(sidebar_accent_foreground),
        sw!(sidebar_border),
        sw!(sidebar_foreground),
        sw!(sidebar_primary),
        sw!(sidebar_primary_foreground),
        sw!(skeleton),
        sw!(slider_bar),
        sw!(slider_thumb),
        sw!(success),
        sw!(success_foreground),
        sw!(success_hover),
        sw!(success_active),
        sw!(switch),
        sw!(switch_thumb),
        sw!(tab),
        sw!(tab_active),
        sw!(tab_active_foreground),
        sw!(tab_bar),
        sw!(tab_bar_segmented),
        sw!(tab_foreground),
        sw!(table),
        sw!(table_active),
        sw!(table_active_border),
        sw!(table_even),
        sw!(table_head),
        sw!(table_head_foreground),
        sw!(table_foot),
        sw!(table_foot_foreground),
        sw!(table_hover),
        sw!(table_row_border),
        sw!(title_bar),
        sw!(title_bar_border),
        sw!(tiles),
        sw!(warning),
        sw!(warning_active),
        sw!(warning_hover),
        sw!(warning_foreground),
        sw!(overlay),
        sw!(window_border),
        sw!(red),
        sw!(red_light),
        sw!(green),
        sw!(green_light),
        sw!(blue),
        sw!(blue_light),
        sw!(yellow),
        sw!(yellow_light),
        sw!(magenta),
        sw!(magenta_light),
        sw!(cyan),
        sw!(cyan_light),
    ];

    h_flex()
        .flex_wrap()
        .gap_3()
        .children(cells)
        .into_any_element()
}

fn swatch_cell(name: &'static str, color: Hsla, border: Hsla) -> AnyElement {
    v_flex()
        .w(px(150.))
        .gap_1()
        .child(
            div()
                .h(px(44.))
                .w_full()
                .rounded(px(6.))
                .border_1()
                .border_color(border)
                .bg(color),
        )
        .child(div().text_xs().child(name))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

pub(super) fn render_button(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_4()
        .child(
            h_flex()
                .gap_3()
                .flex_wrap()
                .child(Button::new("b-primary").label("Primary").primary())
                .child(Button::new("b-secondary").label("Secondary").secondary())
                .child(Button::new("b-danger").label("Danger").danger())
                .child(Button::new("b-warning").label("Warning").warning())
                .child(Button::new("b-success").label("Success").success())
                .child(Button::new("b-info").label("Info").info()),
        )
        .child(
            h_flex()
                .gap_3()
                .flex_wrap()
                .child(Button::new("b-ghost").label("Ghost").ghost())
                .child(Button::new("b-link").label("Link").link())
                .child(Button::new("b-outline").label("Outline").outline())
                .child(
                    Button::new("b-icon")
                        .icon(IconName::Check)
                        .label("With icon")
                        .primary(),
                )
                .child(
                    Button::new("b-loading")
                        .label("Loading")
                        .primary()
                        .loading(true),
                )
                .child(
                    Button::new("b-disabled")
                        .label("Disabled")
                        .primary()
                        .disabled(true),
                ),
        )
        .into_any_element()
}

pub(super) fn render_dropdown_button(_window: &mut Window, _cx: &mut App) -> AnyElement {
    h_flex()
        .gap_3()
        .flex_wrap()
        .child(
            DropdownButton::new("dd-primary")
                .button(Button::new("dd-primary-btn").label("Actions").primary())
                .dropdown_menu(|menu, _, _| {
                    menu.menu("Copy", Box::new(DemoCopy))
                        .menu("Paste", Box::new(DemoPaste))
                        .separator()
                        .menu("Delete", Box::new(DemoDelete))
                }),
        )
        .child(
            DropdownButton::new("dd-outline")
                .button(Button::new("dd-outline-btn").label("More"))
                .outline()
                .dropdown_menu(|menu, _, _| {
                    menu.menu("Copy", Box::new(DemoCopy))
                        .menu("Paste", Box::new(DemoPaste))
                }),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Text inputs (text / password / number)
// ---------------------------------------------------------------------------

struct InputDemo {
    text: Entity<InputState>,
    masked: Entity<InputState>,
    number: Entity<InputState>,
    disabled: Entity<InputState>,
}

impl InputDemo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            text: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Type here...")
                    .default_value("Hello world")
            }),
            masked: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Password")
                    .masked(true)
                    .default_value("secret")
            }),
            number: cx.new(|cx| InputState::new(window, cx).default_value("42")),
            disabled: cx.new(|cx| InputState::new(window, cx).default_value("Read-only")),
        }
    }
}

impl Render for InputDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_4()
            .max_w(px(420.))
            .child(
                Input::new(&self.text)
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search)),
            )
            .child(Input::new(&self.masked).mask_toggle())
            .child(NumberInput::new(&self.number).suffix(Icon::new(IconName::Cpu)))
            .child(Input::new(&self.disabled).disabled(true))
    }
}

pub(super) fn build_input(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| InputDemo::new(window, cx)).into()
}

// ---------------------------------------------------------------------------
// OTP input
// ---------------------------------------------------------------------------

struct OtpDemo {
    code: Entity<OtpState>,
    masked: Entity<OtpState>,
}

impl OtpDemo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            code: cx.new(|cx| OtpState::new(6, window, cx).default_value("123")),
            masked: cx.new(|cx| {
                OtpState::new(4, window, cx)
                    .masked(true)
                    .default_value("12")
            }),
        }
    }
}

impl Render for OtpDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_6()
            .child(OtpInput::new(&self.code).groups(2))
            .child(OtpInput::new(&self.masked).groups(2))
    }
}

pub(super) fn build_otp(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| OtpDemo::new(window, cx)).into()
}

// ---------------------------------------------------------------------------
// Textarea (multi-line input)
// ---------------------------------------------------------------------------

struct TextareaDemo {
    body: Entity<InputState>,
}

impl TextareaDemo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            body: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .placeholder("Write a message...")
                    .default_value("First line\nSecond line")
            }),
        }
    }
}

impl Render for TextareaDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(520.))
            .child(Input::new(&self.body).h(px(180.)))
    }
}

pub(super) fn build_textarea(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| TextareaDemo::new(window, cx)).into()
}

// ---------------------------------------------------------------------------
// Label
// ---------------------------------------------------------------------------

pub(super) fn render_label(_window: &mut Window, cx: &mut App) -> AnyElement {
    v_flex()
        .gap_3()
        .child(Label::new("A simple label"))
        .child(Label::new("Email").secondary("(required)"))
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child("Plain muted helper text"),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Checkbox / Radio / Switch
// ---------------------------------------------------------------------------

pub(super) fn render_checkbox(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_3()
        .child(Checkbox::new("cb-1").label("Unchecked"))
        .child(Checkbox::new("cb-2").label("Checked").checked(true))
        .child(
            Checkbox::new("cb-3")
                .label("Disabled")
                .checked(true)
                .disabled(true),
        )
        .into_any_element()
}

pub(super) fn render_radio(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_4()
        .child(
            v_flex()
                .gap_2()
                .child(Radio::new("rd-1").label("Option A").checked(true))
                .child(Radio::new("rd-2").label("Option B"))
                .child(Radio::new("rd-3").label("Disabled").disabled(true)),
        )
        .child(
            RadioGroup::horizontal("rd-group")
                .children(["Daily", "Weekly", "Monthly"])
                .selected_index(Some(1)),
        )
        .into_any_element()
}

pub(super) fn render_switch(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_3()
        .child(Switch::new("sw-1").label("Off"))
        .child(Switch::new("sw-2").label("On").checked(true))
        .child(
            Switch::new("sw-3")
                .label("Disabled")
                .checked(true)
                .disabled(true),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Select
// ---------------------------------------------------------------------------

struct SelectDemo {
    fruit: Entity<SelectState<SearchableVec<&'static str>>>,
}

impl SelectDemo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let items = SearchableVec::new(vec![
            "Apple",
            "Banana",
            "Cherry",
            "Dragonfruit",
            "Elderberry",
        ]);
        Self {
            fruit: cx.new(|cx| {
                SelectState::new(items, Some(IndexPath::new(0)), window, cx).searchable(true)
            }),
        }
    }
}

impl Render for SelectDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().max_w(px(420.)).child(
            Select::new(&self.fruit)
                .placeholder("Choose a fruit")
                .cleanable(true)
                .w(px(320.)),
        )
    }
}

pub(super) fn build_select(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| SelectDemo::new(window, cx)).into()
}

// ---------------------------------------------------------------------------
// Combobox
// ---------------------------------------------------------------------------

struct ComboboxDemo {
    langs: Entity<ComboboxState<SearchableVec<&'static str>>>,
}

impl ComboboxDemo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let items = SearchableVec::new(vec!["Rust", "Go", "TypeScript", "Python", "Zig"]);
        Self {
            langs: cx.new(|cx| ComboboxState::new(items, vec![], window, cx).searchable(true)),
        }
    }
}

impl Render for ComboboxDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(420.))
            .child(Combobox::new(&self.langs).placeholder("Pick a language"))
    }
}

pub(super) fn build_combobox(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| ComboboxDemo::new(window, cx)).into()
}

// ---------------------------------------------------------------------------
// Form
// ---------------------------------------------------------------------------

struct FormDemo {
    name: Entity<InputState>,
    email: Entity<InputState>,
}

impl FormDemo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Jane Doe")),
            email: cx.new(|cx| InputState::new(window, cx).placeholder("jane@example.com")),
        }
    }
}

impl Render for FormDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().max_w(px(520.)).child(
            v_form()
                .child(
                    field()
                        .label("Name")
                        .required(true)
                        .child(Input::new(&self.name)),
                )
                .child(
                    field()
                        .label("Email")
                        .description("We never share your email.")
                        .child(Input::new(&self.email)),
                )
                .child(
                    field()
                        .label("Subscribe")
                        .child(Switch::new("form-subscribe").checked(true)),
                ),
        )
    }
}

pub(super) fn build_form(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| FormDemo::new(window, cx)).into()
}

// ---------------------------------------------------------------------------
// Slider
// ---------------------------------------------------------------------------

struct SliderDemo {
    volume: Entity<SliderState>,
    range: Entity<SliderState>,
}

impl SliderDemo {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            volume: cx.new(|_| {
                SliderState::new()
                    .min(0.)
                    .max(100.)
                    .step(1.)
                    .default_value(40.0_f32)
            }),
            range: cx.new(|_| {
                SliderState::new()
                    .min(0.)
                    .max(100.)
                    .step(5.)
                    .default_value((20.0_f32, 80.0_f32))
            }),
        }
    }
}

impl Render for SliderDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_6()
            .max_w(px(420.))
            .child(Slider::new(&self.volume).horizontal())
            .child(Slider::new(&self.range).horizontal())
    }
}

pub(super) fn build_slider(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| SliderDemo::new(window, cx)).into()
}

// ---------------------------------------------------------------------------
// Progress / Spinner / Skeleton
// ---------------------------------------------------------------------------

pub(super) fn render_progress(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_4()
        .max_w(px(420.))
        .child(Progress::new("p-25").value(25.))
        .child(Progress::new("p-60").value(60.))
        .child(Progress::new("p-100").value(100.))
        .into_any_element()
}

pub(super) fn render_spinner(_window: &mut Window, cx: &mut App) -> AnyElement {
    h_flex()
        .gap_6()
        .items_center()
        .child(Spinner::new().small())
        .child(Spinner::new())
        .child(Spinner::new().large())
        .child(Spinner::new().large().color(cx.theme().primary))
        .into_any_element()
}

pub(super) fn render_skeleton(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_3()
        .max_w(px(420.))
        .child(Skeleton::new().w(px(360.)).h(px(16.)).rounded(px(4.)))
        .child(Skeleton::new().w(px(280.)).h(px(16.)).rounded(px(4.)))
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(Skeleton::new().w(px(48.)).h(px(48.)).rounded_full())
                .child(
                    v_flex()
                        .gap_2()
                        .child(Skeleton::new().w(px(160.)).h(px(12.)).rounded(px(4.)))
                        .child(Skeleton::new().w(px(120.)).h(px(12.)).rounded(px(4.))),
                ),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Badge / Tag
// ---------------------------------------------------------------------------

pub(super) fn render_badge(_window: &mut Window, _cx: &mut App) -> AnyElement {
    h_flex()
        .gap_6()
        .items_center()
        .child(
            Badge::new()
                .count(3)
                .child(Button::new("badge-bell").icon(IconName::Bell).ghost()),
        )
        .child(
            Badge::new()
                .count(128)
                .max(99)
                .child(Button::new("badge-inbox").icon(IconName::Inbox).ghost()),
        )
        .child(
            Badge::new()
                .dot()
                .child(Button::new("badge-dot").icon(IconName::Bell).ghost()),
        )
        .into_any_element()
}

pub(super) fn render_tag(_window: &mut Window, _cx: &mut App) -> AnyElement {
    h_flex()
        .gap_3()
        .flex_wrap()
        .child(Tag::primary().child("Primary"))
        .child(Tag::secondary().child("Secondary"))
        .child(Tag::success().child("Success"))
        .child(Tag::warning().child("Warning"))
        .child(Tag::danger().child("Danger"))
        .child(Tag::info().child("Info"))
        .child(Tag::primary().outline().child("Outline"))
        .child(Tag::secondary().rounded_full().child("Rounded"))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Alert
// ---------------------------------------------------------------------------

pub(super) fn render_alert(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_3()
        .max_w(px(560.))
        .child(
            Alert::info("a-info", "A new software update is available.").title("Update available"),
        )
        .child(Alert::success("a-success", "Your changes have been saved.").title("Saved"))
        .child(Alert::warning("a-warning", "Your session is about to expire.").title("Heads up"))
        .child(
            Alert::error("a-error", "Could not connect to the server.").title("Connection error"),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Notification (push-only API: buttons that push to the window's list)
// ---------------------------------------------------------------------------

pub(super) fn render_notification(_window: &mut Window, cx: &mut App) -> AnyElement {
    v_flex()
        .gap_3()
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child("Click a button to push a notification (appears top-right)."),
        )
        .child(
            h_flex()
                .gap_3()
                .flex_wrap()
                .child(
                    Button::new("n-info")
                        .label("Info")
                        .info()
                        .on_click(|_, window, cx| {
                            window.push_notification(
                                (NotificationType::Info, "This is an info notification."),
                                cx,
                            );
                        }),
                )
                .child(
                    Button::new("n-success")
                        .label("Success")
                        .success()
                        .on_click(|_, window, cx| {
                            window.push_notification(
                                (NotificationType::Success, "Saved successfully."),
                                cx,
                            );
                        }),
                )
                .child(
                    Button::new("n-warning")
                        .label("Warning")
                        .warning()
                        .on_click(|_, window, cx| {
                            window.push_notification(
                                (NotificationType::Warning, "Low disk space."),
                                cx,
                            );
                        }),
                )
                .child(
                    Button::new("n-error")
                        .label("Error")
                        .danger()
                        .on_click(|_, window, cx| {
                            window.push_notification(
                                (NotificationType::Error, "Something went wrong."),
                                cx,
                            );
                        }),
                ),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Tooltip
// ---------------------------------------------------------------------------

pub(super) fn render_tooltip(_window: &mut Window, _cx: &mut App) -> AnyElement {
    h_flex()
        .gap_4()
        .child(
            Button::new("tt-1")
                .label("Hover me")
                .primary()
                .tooltip("This is a tooltip."),
        )
        .child(
            Button::new("tt-2")
                .label("Search")
                .icon(IconName::Search)
                .tooltip("Search the gallery"),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Rating
// ---------------------------------------------------------------------------

pub(super) fn render_rating(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_3()
        .child(Rating::new("rt-2").value(2).max(5))
        .child(Rating::new("rt-3").value(3).max(5))
        .child(Rating::new("rt-disabled").value(4).max(5).disabled(true))
        .into_any_element()
}

// ===========================================================================
// Part 2 (#125): layout, navigation, overlay and picker components.
// ===========================================================================

// ---------------------------------------------------------------------------
// Separator
// ---------------------------------------------------------------------------

pub(super) fn render_separator(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_4()
        .max_w(px(420.))
        .child("Above the line")
        .child(Separator::horizontal())
        .child(Separator::horizontal().label("With label"))
        .child(Separator::horizontal_dashed())
        .child(
            h_flex()
                .h(px(40.))
                .gap_3()
                .items_center()
                .child("Left")
                .child(Separator::vertical())
                .child("Right"),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Group box
// ---------------------------------------------------------------------------

pub(super) fn render_group_box(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_4()
        .max_w(px(480.))
        .child(
            GroupBox::new()
                .title("Appearance")
                .child(Switch::new("gb-dark").label("Dark mode").checked(true))
                .child(Switch::new("gb-compact").label("Compact layout")),
        )
        .child(
            GroupBox::new()
                .title("Notifications")
                .fill()
                .child(Checkbox::new("gb-email").label("Email").checked(true))
                .child(Checkbox::new("gb-push").label("Push")),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Description list
// ---------------------------------------------------------------------------

pub(super) fn render_description_list(_window: &mut Window, _cx: &mut App) -> AnyElement {
    // `DescriptionList` implements `Sizable` but not `Styled`, so width is
    // constrained on a wrapper, matching how the other demos size their container.
    div()
        .max_w(px(560.))
        .child(
            DescriptionList::new()
                .columns(2)
                .bordered(true)
                .child(DescriptionItem::new("Name").value("Jane Doe"))
                .child(DescriptionItem::new("Role").value("Maintainer"))
                .child(
                    DescriptionItem::new("Email")
                        .value("jane@example.com")
                        .span(2),
                )
                .child(DescriptionItem::new("Location").value("Berlin"))
                .child(DescriptionItem::new("Timezone").value("CET")),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Kbd (keyboard shortcuts)
// ---------------------------------------------------------------------------

pub(super) fn render_kbd(_window: &mut Window, _cx: &mut App) -> AnyElement {
    // Parsing a literal keystroke can only fail on a typo here, so a panic is
    // the right signal during development.
    let key = |s: &str| Kbd::new(Keystroke::parse(s).expect("valid keystroke literal"));
    h_flex()
        .gap_3()
        .flex_wrap()
        .child(key("cmd-shift-p"))
        .child(key("ctrl-c"))
        .child(key("enter"))
        .child(key("escape").outline())
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Link
// ---------------------------------------------------------------------------

pub(super) fn render_link(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_3()
        .child(
            Link::new("link-repo")
                .href("https://github.com/skrischer/rift")
                .child("rift on GitHub"),
        )
        .child(
            Link::new("link-disabled")
                .disabled(true)
                .child("Disabled link"),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Icon
// ---------------------------------------------------------------------------

pub(super) fn render_icon(_window: &mut Window, cx: &mut App) -> AnyElement {
    let names = [
        IconName::Search,
        IconName::Check,
        IconName::Bell,
        IconName::Inbox,
        IconName::Heart,
        IconName::Star,
        IconName::Info,
        IconName::Github,
        IconName::User,
        IconName::Cpu,
    ];
    v_flex()
        .gap_4()
        .child(
            h_flex()
                .gap_4()
                .flex_wrap()
                .children(names.into_iter().map(Icon::new)),
        )
        .child(
            h_flex()
                .gap_4()
                .items_center()
                .child(Icon::new(IconName::Heart).small())
                .child(Icon::new(IconName::Heart))
                .child(Icon::new(IconName::Heart).large())
                .child(
                    Icon::new(IconName::Heart)
                        .with_size(px(40.))
                        .text_color(cx.theme().primary),
                ),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Avatar
// ---------------------------------------------------------------------------

pub(super) fn render_avatar(_window: &mut Window, _cx: &mut App) -> AnyElement {
    // Initials mode (no `.src()`) so the demo needs no network or embedded image.
    h_flex()
        .gap_4()
        .items_center()
        .child(Avatar::new().name("Jane Doe").small())
        .child(Avatar::new().name("Sebastian Krischer"))
        .child(Avatar::new().name("Rift Dev").large())
        .child(Avatar::new().placeholder(IconName::User))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Image
// ---------------------------------------------------------------------------

fn image_tile(icon: IconName, border: Hsla, bg: Hsla, fg: Hsla) -> AnyElement {
    h_flex()
        .size(px(120.))
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .child(Icon::new(icon).with_size(px(56.)).text_color(fg))
        .into_any_element()
}

pub(super) fn render_image(_window: &mut Window, cx: &mut App) -> AnyElement {
    let border = cx.theme().border;
    let bg = cx.theme().muted;
    let fg = cx.theme().muted_foreground;
    v_flex()
        .gap_3()
        .child(div().text_color(fg).child(
            "gpui's img() renders raster or remote image sources (remote URLs need an \
             http client); this offline gallery build embeds only vector icons, shown \
             here as framed tiles.",
        ))
        .child(
            h_flex()
                .gap_4()
                .child(image_tile(IconName::Globe, border, bg, fg))
                .child(image_tile(IconName::Map, border, bg, fg))
                .child(image_tile(IconName::File, border, bg, fg)),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Breadcrumb
// ---------------------------------------------------------------------------

pub(super) fn render_breadcrumb(_window: &mut Window, _cx: &mut App) -> AnyElement {
    Breadcrumb::new()
        .child("Home")
        .child("Projects")
        .child(BreadcrumbItem::new("rift"))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

pub(super) fn render_sidebar(_window: &mut Window, cx: &mut App) -> AnyElement {
    let border = cx.theme().border;
    div()
        .h(px(360.))
        .w(px(260.))
        .border_1()
        .border_color(border)
        .rounded(px(6.))
        .overflow_hidden()
        .child(
            Sidebar::new("sidebar-demo")
                .w(relative(1.))
                .border_0()
                .child(
                    SidebarGroup::new("Workspace").child(
                        SidebarMenu::new()
                            .child(
                                SidebarMenuItem::new("Explorer")
                                    .icon(IconName::Folder)
                                    .active(true),
                            )
                            .child(SidebarMenuItem::new("Search").icon(IconName::Search))
                            .child(SidebarMenuItem::new("Browse").icon(IconName::Globe)),
                    ),
                )
                .child(
                    SidebarGroup::new("Account").child(
                        SidebarMenu::new()
                            .child(SidebarMenuItem::new("Profile").icon(IconName::User))
                            .child(SidebarMenuItem::new("Notifications").icon(IconName::Bell))
                            .child(SidebarMenuItem::new("Preferences").icon(IconName::Settings2)),
                    ),
                ),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Menu (standalone dropdown + context menu)
// ---------------------------------------------------------------------------

pub(super) fn render_menu(_window: &mut Window, cx: &mut App) -> AnyElement {
    let border = cx.theme().border;
    v_flex()
        .gap_6()
        .child(
            Button::new("menu-dropdown")
                .outline()
                .label("Open menu")
                .dropdown_menu(|menu, _, _| {
                    menu.menu("Copy", Box::new(DemoCopy))
                        .menu("Paste", Box::new(DemoPaste))
                        .separator()
                        .menu("Delete", Box::new(DemoDelete))
                }),
        )
        .child(
            div()
                .id("menu-context")
                .w(px(280.))
                .p_4()
                .border_1()
                .border_color(border)
                .rounded(px(6.))
                .child("Right-click anywhere in this box")
                .context_menu(|menu, _, _| {
                    menu.menu("Copy", Box::new(DemoCopy))
                        .menu("Paste", Box::new(DemoPaste))
                        .separator()
                        .menu("Delete", Box::new(DemoDelete))
                }),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Popover
// ---------------------------------------------------------------------------

pub(super) fn render_popover(_window: &mut Window, _cx: &mut App) -> AnyElement {
    h_flex()
        .gap_4()
        .child(
            Popover::new("popover-demo")
                .trigger(
                    Button::new("popover-trigger")
                        .outline()
                        .label("Open popover"),
                )
                .content(|_, _, _| {
                    v_flex()
                        .gap_2()
                        .p_2()
                        .child("Popover title")
                        .child("Any content can live inside a popover.")
                }),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Hover card
// ---------------------------------------------------------------------------

pub(super) fn render_hover_card(_window: &mut Window, _cx: &mut App) -> AnyElement {
    h_flex()
        .gap_4()
        .child(
            HoverCard::new("hover-card-demo")
                .trigger(
                    Button::new("hover-card-trigger")
                        .outline()
                        .label("Hover me"),
                )
                .content(|_, _, _| {
                    v_flex()
                        .gap_1()
                        .p_2()
                        .child("Hover card")
                        .child("Shown on hover after a short delay.")
                }),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Dialog (modal opened from a trigger)
// ---------------------------------------------------------------------------

pub(super) fn render_dialog(_window: &mut Window, _cx: &mut App) -> AnyElement {
    h_flex()
        .gap_4()
        .child(
            Button::new("dialog-open")
                .primary()
                .label("Open dialog")
                .on_click(|_, window, cx| {
                    window.open_dialog(cx, |dialog, _, _| {
                        dialog
                            .title("Delete file?")
                            .child("This action cannot be undone.")
                            .on_ok(|_, _, _| true)
                            .on_cancel(|_, _, _| true)
                    });
                }),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Sheet (side drawer opened from a trigger)
// ---------------------------------------------------------------------------

pub(super) fn render_sheet(_window: &mut Window, _cx: &mut App) -> AnyElement {
    h_flex()
        .gap_4()
        .child(
            Button::new("sheet-open")
                .primary()
                .label("Open sheet")
                .on_click(|_, window, cx| {
                    window.open_sheet(cx, |sheet, _, _| {
                        sheet.title("Details").child(
                            div()
                                .p_2()
                                .child("A sheet slides in from the edge of the window."),
                        )
                    });
                }),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------

pub(super) fn render_clipboard(_window: &mut Window, _cx: &mut App) -> AnyElement {
    h_flex()
        .gap_3()
        .items_center()
        .child("cargo install rift")
        .child(
            Clipboard::new("clipboard-demo")
                .value("cargo install rift")
                .on_copied(|_, _, _| {}),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Scrollbar
// ---------------------------------------------------------------------------

pub(super) fn render_scrollbar(_window: &mut Window, cx: &mut App) -> AnyElement {
    let border = cx.theme().border;
    div()
        .id("scrollbar-demo")
        .h(px(220.))
        .max_w(px(420.))
        .border_1()
        .border_color(border)
        .rounded(px(6.))
        .p_3()
        .overflow_y_scrollbar()
        .child(
            v_flex()
                .gap_2()
                .children((1..=40).map(|i| div().child(format!("Row {i}")))),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Resizable
// ---------------------------------------------------------------------------

pub(super) fn render_resizable(_window: &mut Window, cx: &mut App) -> AnyElement {
    let border = cx.theme().border;
    div()
        .h(px(240.))
        .border_1()
        .border_color(border)
        .rounded(px(6.))
        .overflow_hidden()
        .child(
            h_resizable("resizable-demo")
                .child(
                    resizable_panel()
                        .size(px(160.))
                        .size_range(px(120.)..px(280.))
                        .child(div().p_3().child("Sidebar")),
                )
                .child(resizable_panel().child(div().p_3().child("Main content"))),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

struct TabsDemo {
    active: usize,
}

impl Render for TabsDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let labels = ["Account", "Password", "Notifications"];
        v_flex()
            .gap_3()
            .child(
                TabBar::new("tabs-demo")
                    .selected_index(self.active)
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.active = *ix;
                        cx.notify();
                    }))
                    .children(labels.into_iter().map(|l| Tab::new().label(l))),
            )
            .child(
                div()
                    .p_2()
                    .child(format!("Content for the {} tab.", labels[self.active])),
            )
    }
}

pub(super) fn build_tabs(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| TabsDemo { active: 0 }).into()
}

// ---------------------------------------------------------------------------
// Accordion
// ---------------------------------------------------------------------------

struct AccordionDemo {
    open: Vec<usize>,
}

impl Render for AccordionDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let o0 = self.open.contains(&0);
        let o1 = self.open.contains(&1);
        let o2 = self.open.contains(&2);
        let this = cx.entity().downgrade();
        Accordion::new("accordion-demo")
            .multiple(true)
            .item(move |item| {
                item.open(o0)
                    .title("What is rift?")
                    .child("An agent-centric IDE built in Rust.")
            })
            .item(move |item| {
                item.open(o1)
                    .title("Is it open source?")
                    .child("Yes, always free and open source.")
            })
            .item(move |item| {
                item.open(o2)
                    .title("Which agents does it support?")
                    .child("Any terminal coding agent, unmodified.")
            })
            .on_toggle_click(move |open_ixs: &[usize], _window, cx| {
                let open = open_ixs.to_vec();
                let _ = this.update(cx, |this, cx| {
                    this.open = open;
                    cx.notify();
                });
            })
    }
}

pub(super) fn build_accordion(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| AccordionDemo { open: vec![0] }).into()
}

// ---------------------------------------------------------------------------
// Collapsible
// ---------------------------------------------------------------------------

struct CollapsibleDemo {
    open: bool,
}

impl Render for CollapsibleDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(
                Button::new("collapsible-toggle")
                    .outline()
                    .label(if self.open {
                        "Hide details"
                    } else {
                        "Show details"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open = !this.open;
                        cx.notify();
                    })),
            )
            .child(
                Collapsible::new().open(self.open).content(
                    v_flex()
                        .gap_1()
                        .p_2()
                        .child("These details are revealed when expanded.")
                        .child("Collapsible is controlled by a parent boolean."),
                ),
            )
    }
}

pub(super) fn build_collapsible(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| CollapsibleDemo { open: true }).into()
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

struct PaginationDemo {
    page: usize,
}

impl Render for PaginationDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(
                Pagination::new("pagination-demo")
                    .total_pages(10)
                    .current_page(self.page)
                    .on_click(cx.listener(|this, page: &usize, _, cx| {
                        this.page = *page;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Page {} of 10", self.page)),
            )
    }
}

pub(super) fn build_pagination(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| PaginationDemo { page: 1 }).into()
}

// ---------------------------------------------------------------------------
// Stepper
// ---------------------------------------------------------------------------

struct StepperDemo {
    step: usize,
}

impl Render for StepperDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_4()
            .max_w(px(560.))
            .child(
                Stepper::new("stepper-demo")
                    .selected_index(self.step)
                    .items([
                        StepperItem::new().child("Account"),
                        StepperItem::new().child("Profile"),
                        StepperItem::new().child("Confirm"),
                    ])
                    .on_click(cx.listener(|this, step: &usize, _, cx| {
                        this.step = *step;
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("stepper-back")
                            .outline()
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step = this.step.saturating_sub(1);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("stepper-next")
                            .primary()
                            .label("Next")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.step < 2 {
                                    this.step += 1;
                                }
                                cx.notify();
                            })),
                    ),
            )
    }
}

pub(super) fn build_stepper(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| StepperDemo { step: 0 }).into()
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

struct GalleryListDelegate {
    items: Vec<SharedString>,
    selected: Option<usize>,
}

impl ListDelegate for GalleryListDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let label = self.items.get(ix.row)?;
        Some(
            ListItem::new(("list-item", ix.row))
                .selected(self.selected == Some(ix.row))
                .child(label.clone()),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix.map(|ix| ix.row);
        cx.notify();
    }
}

struct ListDemo {
    list: Entity<ListState<GalleryListDelegate>>,
}

impl ListDemo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let delegate = GalleryListDelegate {
            items: vec![
                "Inbox".into(),
                "Drafts".into(),
                "Sent".into(),
                "Archive".into(),
                "Spam".into(),
                "Trash".into(),
            ],
            selected: Some(0),
        };
        Self {
            list: cx.new(|cx| ListState::new(delegate, window, cx)),
        }
    }
}

impl Render for ListDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .h(px(280.))
            .max_w(px(360.))
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(6.))
            .child(List::new(&self.list).flex_1().w_full())
    }
}

pub(super) fn build_list(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| ListDemo::new(window, cx)).into()
}

// ---------------------------------------------------------------------------
// Calendar
// ---------------------------------------------------------------------------

struct CalendarDemo {
    state: Entity<CalendarState>,
}

impl Render for CalendarDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().child(Calendar::new(&self.state).number_of_months(1))
    }
}

pub(super) fn build_calendar(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| CalendarDemo {
        state: cx.new(|cx| CalendarState::new(window, cx)),
    })
    .into()
}

// ---------------------------------------------------------------------------
// Date picker
// ---------------------------------------------------------------------------

struct DatePickerDemo {
    single: Entity<DatePickerState>,
    range: Entity<DatePickerState>,
}

impl Render for DatePickerDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_4()
            .max_w(px(320.))
            .child(
                DatePicker::new(&self.single)
                    .placeholder("Pick a date")
                    .cleanable(true),
            )
            .child(
                DatePicker::new(&self.range)
                    .placeholder("Pick a range")
                    .number_of_months(2),
            )
    }
}

pub(super) fn build_date_picker(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| DatePickerDemo {
        single: cx.new(|cx| DatePickerState::new(window, cx)),
        range: cx.new(|cx| DatePickerState::range(window, cx)),
    })
    .into()
}

// ---------------------------------------------------------------------------
// Color picker
// ---------------------------------------------------------------------------

struct ColorPickerDemo {
    color: Entity<ColorPickerState>,
}

impl Render for ColorPickerDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        ColorPicker::new(&self.color).label("Pick a color")
    }
}

pub(super) fn build_color_picker(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| {
        let primary = cx.theme().primary;
        ColorPickerDemo {
            color: cx.new(|cx| ColorPickerState::new(window, cx).default_value(primary)),
        }
    })
    .into()
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

struct SettingsDemo {
    dark: bool,
    telemetry: bool,
    theme_name: SharedString,
    font_size: f64,
}

impl Render for SettingsDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        Settings::new("settings-demo").pages(vec![SettingPage::new("General")
            .icon(IconName::Settings2)
            .default_open(true)
            .groups(vec![
                SettingGroup::new().title("Appearance").items(vec![
                    SettingItem::new(
                        "Dark mode",
                        SettingField::switch(
                            {
                                let this = this.clone();
                                move |cx: &App| this.read(cx).dark
                            },
                            {
                                let this = this.clone();
                                move |value: bool, cx: &mut App| {
                                    this.update(cx, |demo, cx| {
                                        demo.dark = value;
                                        cx.notify();
                                    });
                                }
                            },
                        ),
                    )
                    .description("Use the dark color scheme."),
                    SettingItem::new(
                        "Theme",
                        SettingField::dropdown(
                            vec![
                                ("mocha".into(), "Mocha".into()),
                                ("latte".into(), "Latte".into()),
                            ],
                            {
                                let this = this.clone();
                                move |cx: &App| this.read(cx).theme_name.clone()
                            },
                            {
                                let this = this.clone();
                                move |value: SharedString, cx: &mut App| {
                                    this.update(cx, |demo, cx| {
                                        demo.theme_name = value;
                                        cx.notify();
                                    });
                                }
                            },
                        ),
                    ),
                ]),
                SettingGroup::new().title("Privacy").items(vec![
                    SettingItem::new(
                        "Telemetry",
                        SettingField::checkbox(
                            {
                                let this = this.clone();
                                move |cx: &App| this.read(cx).telemetry
                            },
                            {
                                let this = this.clone();
                                move |value: bool, cx: &mut App| {
                                    this.update(cx, |demo, cx| {
                                        demo.telemetry = value;
                                        cx.notify();
                                    });
                                }
                            },
                        ),
                    ),
                    SettingItem::new(
                        "Font size",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 8.0,
                                max: 32.0,
                                step: 1.0,
                            },
                            {
                                let this = this.clone();
                                move |cx: &App| this.read(cx).font_size
                            },
                            {
                                let this = this.clone();
                                move |value: f64, cx: &mut App| {
                                    this.update(cx, |demo, cx| {
                                        demo.font_size = value;
                                        cx.notify();
                                    });
                                }
                            },
                        ),
                    ),
                ]),
            ])])
    }
}

pub(super) fn build_settings(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| SettingsDemo {
        dark: true,
        telemetry: false,
        theme_name: "mocha".into(),
        font_size: 14.0,
    })
    .into()
}

// ---------------------------------------------------------------------------
// Chart
// ---------------------------------------------------------------------------

/// A datum for the chart demos: a category label and its numeric value. Charts
/// map each datum to its axes via closures, so a tiny owned struct keeps them
/// trivial. Values are plain `f64`, so the gpui-component `decimal` feature is
/// intentionally not enabled.
struct ChartPoint {
    label: SharedString,
    value: f64,
}

fn chart_data() -> Vec<ChartPoint> {
    [
        ("Jan", 186.0),
        ("Feb", 305.0),
        ("Mar", 237.0),
        ("Apr", 173.0),
        ("May", 209.0),
        ("Jun", 264.0),
    ]
    .into_iter()
    .map(|(label, value)| ChartPoint {
        label: label.into(),
        value,
    })
    .collect()
}

/// Wrap a chart in a fixed-height, bordered card. Charts fill their parent, so
/// they need a sized container to lay out against.
fn chart_card(title: &str, cx: &App, chart: impl IntoElement) -> impl IntoElement {
    v_flex()
        .h(px(320.))
        .p_4()
        .gap_2()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(px(8.))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(title.to_string()),
        )
        .child(div().flex_1().child(chart))
}

pub(super) fn render_chart(_window: &mut Window, cx: &mut App) -> AnyElement {
    let bar_color = cx.theme().chart_1;
    let line_color = cx.theme().chart_2;
    v_flex()
        .gap_6()
        .child(chart_card(
            "Bar chart",
            cx,
            BarChart::new(chart_data())
                .band(|d| d.label.clone())
                .value(|d| d.value)
                .label(|d| d.value.to_string())
                .corner_radii(px(6.))
                .fill(move |_, _, _, _| bar_color),
        ))
        .child(chart_card(
            "Line chart",
            cx,
            LineChart::new(chart_data())
                .x(|d| d.label.clone())
                .y(|d| d.value)
                .stroke(line_color)
                .dot(),
        ))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Code editor
// ---------------------------------------------------------------------------

const EDITOR_SAMPLE: &str = "\
use gpui_component::input::{Input, InputState};

/// Rendered by the gallery's code-editor demo with Rust syntax highlighting
/// (the `gallery` feature enables a single `tree-sitter-rust` grammar).
fn greet(name: &str) -> String {
    format!(\"Hello, {name}!\")
}

fn main() {
    for name in [\"rift\", \"gpui\", \"tmux\"] {
        println!(\"{}\", greet(name));
    }
}
";

struct CodeEditorDemo {
    state: Entity<InputState>,
}

pub(super) fn build_code_editor(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| CodeEditorDemo {
        state: cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("rust")
                .multi_line(true)
                .line_number(true)
                .tab_size(TabSize {
                    tab_size: 4,
                    ..Default::default()
                })
                .default_value(EDITOR_SAMPLE)
        }),
    })
    .into()
}

impl Render for CodeEditorDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().h(px(420.)).child(
            Input::new(&self.state)
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(cx.theme().mono_font_size)
                .size_full(),
        )
    }
}

// ---------------------------------------------------------------------------
// Table (static)
// ---------------------------------------------------------------------------

pub(super) fn render_table(_window: &mut Window, _cx: &mut App) -> AnyElement {
    let rows = [
        ("rift-app", "GPUI application binary", "binary"),
        ("rift-ssh", "SSH connection and PTY stream", "library"),
        ("rift-terminal", "GPUI terminal widget", "library"),
        ("rift-daemon", "Remote host daemon", "binary"),
        ("rift-protocol", "Shared message types", "library"),
    ];
    div()
        .max_w(px(640.))
        .child(
            Table::new()
                .child(
                    TableHeader::new().child(
                        TableRow::new()
                            .child(TableHead::new().child("Crate"))
                            .child(TableHead::new().child("Responsibility"))
                            .child(TableHead::new().child("Kind")),
                    ),
                )
                .child(
                    TableBody::new().children(rows.into_iter().map(|(name, role, kind)| {
                        TableRow::new()
                            .child(TableCell::new().child(name))
                            .child(TableCell::new().child(role))
                            .child(TableCell::new().child(kind))
                    })),
                ),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Data table
// ---------------------------------------------------------------------------

struct CrateRow {
    name: &'static str,
    files: usize,
    lines: usize,
    public: bool,
}

struct GalleryTableDelegate {
    columns: Vec<Column>,
    rows: Vec<CrateRow>,
}

impl TableDelegate for GalleryTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let row = &self.rows[row_ix];
        let text = match col_ix {
            0 => row.name.to_string(),
            1 => row.files.to_string(),
            2 => row.lines.to_string(),
            _ => (if row.public { "yes" } else { "no" }).to_string(),
        };
        div().child(text)
    }
}

struct DataTableDemo {
    state: Entity<TableState<GalleryTableDelegate>>,
}

pub(super) fn build_data_table(window: &mut Window, cx: &mut App) -> AnyView {
    let delegate = GalleryTableDelegate {
        columns: vec![
            Column::new("name", "Crate"),
            Column::new("files", "Files").text_right(),
            Column::new("lines", "Lines").text_right(),
            Column::new("public", "Public API"),
        ],
        rows: vec![
            CrateRow {
                name: "rift-app",
                files: 12,
                lines: 4200,
                public: false,
            },
            CrateRow {
                name: "rift-ssh",
                files: 5,
                lines: 1300,
                public: true,
            },
            CrateRow {
                name: "rift-terminal",
                files: 8,
                lines: 2600,
                public: true,
            },
            CrateRow {
                name: "rift-daemon",
                files: 9,
                lines: 3100,
                public: true,
            },
            CrateRow {
                name: "rift-protocol",
                files: 4,
                lines: 700,
                public: true,
            },
        ],
    };
    cx.new(|cx| DataTableDemo {
        state: cx.new(|cx| TableState::new(delegate, window, cx)),
    })
    .into()
}

impl Render for DataTableDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(360.))
            .child(DataTable::new(&self.state).stripe(true).bordered(true))
    }
}

// ---------------------------------------------------------------------------
// WebView (#127)
// ---------------------------------------------------------------------------

/// Windows: a live embedded browser backed by `gpui-wry` (Wry / WebView2). The
/// native child webview is positioned by the rendered element's bounds, so it is
/// hosted in a fixed-height bordered container.
#[cfg(windows)]
struct WebViewDemo {
    webview: Entity<gpui_wry::WebView>,
}

#[cfg(windows)]
pub(super) fn build_webview(window: &mut Window, cx: &mut App) -> AnyView {
    use raw_window_handle::HasWindowHandle as _;

    let webview = cx.new(|cx| {
        let builder = wry::WebViewBuilder::new();
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        let handle = window
            .window_handle()
            .expect("gallery window exposes a raw window handle");
        let inner = builder
            .build_as_child(&handle)
            .expect("WebView2 runtime builds a child webview");
        let mut view = gpui_wry::WebView::new(inner, window, cx);
        view.load_url("https://longbridge.github.io/gpui-component");
        view
    });
    cx.new(|_| WebViewDemo { webview }).into()
}

#[cfg(windows)]
impl Render for WebViewDemo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(480.))
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(8.))
            .child(self.webview.clone())
    }
}

/// Non-Windows: the real WebView renders only on the Windows sign-off target (the
/// upstream Linux GTK path is non-functional and would pull `libwebkit2gtk` into
/// the headless/CI builds), so other targets show this notice.
#[cfg(not(windows))]
pub(super) fn render_webview(_window: &mut Window, _cx: &mut App) -> AnyElement {
    v_flex()
        .gap_3()
        .max_w(px(560.))
        .child(
            Alert::info(
                "webview-windows-only",
                "The WebView demo embeds a live browser via gpui-wry (Wry / \
                 WebView2) and renders only on the Windows sign-off target. The \
                 upstream Linux path is non-functional, so this build shows a \
                 notice instead.",
            )
            .title("WebView — available on Windows only"),
        )
        .into_any_element()
}
