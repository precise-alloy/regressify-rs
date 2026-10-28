use super::*;

use crate::desktop::workspace::ThemePreference;
use gpui_kit::component::Size;

pub(super) fn release_app_menu(bar: &Entity<gpui_kit::component::menu::AppMenuBar>, cx: &mut App) {
    // Kit 0.7.3 menus own their bar; an empty reload breaks that cycle without changing the registry.
    let menus = gpui_kit::base::GlobalState::global(cx).app_menus().to_vec();
    gpui_kit::base::GlobalState::global_mut(cx).set_app_menus(Vec::new());
    bar.update(cx, |bar, cx| bar.reload(cx));
    gpui_kit::base::GlobalState::global_mut(cx).set_app_menus(menus);
}

type CopiedCallback = Box<dyn Fn(&mut App)>;

#[derive(IntoElement)]
pub(super) struct CopyText {
    id: ElementId,
    value: SharedString,
    label: SharedString,
    size: Size,
    tooltip: Option<SharedString>,
    on_copied: Option<CopiedCallback>,
}

impl CopyText {
    pub(super) fn new(
        id: impl Into<ElementId>,
        value: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            label: label.into(),
            size: Size::Medium,
            tooltip: None,
            on_copied: None,
        }
    }

    pub(super) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub(super) fn on_copied(mut self, callback: impl Fn(&mut App) + 'static) -> Self {
        self.on_copied = Some(Box::new(callback));
        self
    }
}

impl Sizable for CopyText {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for CopyText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let identity =
            window.use_keyed_state(self.id.clone(), cx, |_, _| (SharedString::default(), 0_u64));
        let generation = identity.update(cx, |(value, generation), _| {
            if value != &self.value {
                *value = self.value.clone();
                *generation += 1;
            }
            *generation
        });
        // Feedback suppresses repeated copies; changed text gets a fresh key without putting text in IDs.
        div().id(self.id).test_support().flex_shrink_0().child(
            gpui_kit::component::clipboard::Clipboard::new(("clipboard-value", generation))
                .value(self.value)
                .tooltip(self.tooltip.unwrap_or_else(|| self.label.clone()))
                .accessibility_label(self.label)
                .with_size(self.size)
                .when_some(self.on_copied, |clipboard, callback| {
                    clipboard.on_copied(move |_, _, cx| callback(cx))
                }),
        )
    }
}

pub(super) fn restore_dropdown_menu_focus(
    menu_focus: FocusHandle,
    restore: FocusHandle,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
) {
    let menu = cx.entity();
    window
        .subscribe(&menu, cx, move |_, _: &DismissEvent, window, cx| {
            let menu_focus = menu_focus.clone();
            let restore = restore.clone();
            window.defer(cx, move |window, cx| {
                // Dropdown menu dismissal can leave focus on its disappearing popup; never override a newer focus owner.
                if menu_focus.is_focused(window) || window.focused(cx).is_none() {
                    window.focus(&restore, cx);
                }
            });
        })
        .detach();
}

pub(super) fn restore_managed_menu_focus<View: 'static>(
    owner: WeakEntity<View>,
    menu_focus: FocusHandle,
    restore: FocusHandle,
    invalidate: fn(&mut View),
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
) {
    let menu = cx.entity();
    window
        .subscribe(&menu, cx, move |_, _: &DismissEvent, window, cx| {
            let owner = owner.clone();
            let menu_focus = menu_focus.clone();
            let restore = restore.clone();
            window.defer(cx, move |window, cx| {
                // Managed menu painting can re-focus its closing view; never override a newer focus owner.
                if !menu_focus.is_focused(window) {
                    return;
                }
                let _ = owner.update(cx, |this, cx| {
                    invalidate(this);
                    window.focus(&restore, cx);
                    cx.notify();
                });
            });
        })
        .detach();
}

#[derive(IntoElement)]
pub(super) struct ToolbarFocus {
    id: ElementId,
    toolbar: gpui_kit::component::toolbar::Toolbar,
}

impl ToolbarFocus {
    pub(super) fn new(
        id: impl Into<ElementId>,
        toolbar: gpui_kit::component::toolbar::Toolbar,
    ) -> Self {
        Self {
            id: id.into(),
            toolbar,
        }
    }
}

impl RenderOnce for ToolbarFocus {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| {
                cx.focus_handle().tab_stop(false)
            })
            .read(cx)
            .clone();
        div()
            .id(self.id)
            .w_full()
            .min_w_0()
            .track_focus(&focus)
            .on_key_down(move |event, window, cx| {
                if event.keystroke.modifiers != Modifiers::default()
                    || !focus.contains_focused(window, cx)
                    || gpui_kit::base::GlobalState::is_in_deferred_context(cx)
                {
                    return;
                }
                let backward = match event.keystroke.key.as_str() {
                    "left" => true,
                    "right" => false,
                    _ => return,
                };
                let Some(start) = window.focused(cx) else {
                    return;
                };
                let mut previous = start.clone();
                let mut destination = start.clone();
                // Kit 0.7.3 fails backward wrapping; choose scoped destinations from bounded forward order.
                for _ in 0..1024 {
                    window.focus_next(cx);
                    let Some(next) = window.focused(cx) else {
                        break;
                    };
                    if next == start {
                        window.focus(&destination, cx);
                        cx.stop_propagation();
                        return;
                    }
                    if next == previous {
                        break;
                    }
                    if focus.contains(&next, window) {
                        if !backward {
                            cx.stop_propagation();
                            return;
                        }
                        destination = next.clone();
                    }
                    previous = next;
                }
                window.focus(&start, cx);
                cx.stop_propagation();
            })
            .child(self.toolbar.disabled(true))
    }
}

pub(super) fn titlebar_control(element: impl IntoElement) -> AnyElement {
    div()
        .flex_shrink_0()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(element)
        .into_any_element()
}

pub(super) fn selection_checkbox(
    id: impl Into<ElementId>,
    checked: bool,
    partial: bool,
    disabled: bool,
    cx: &App,
) -> gpui_kit::base::Checkbox {
    let ring = cx.theme().ring;
    gpui_kit::base::Checkbox::new(id)
        .checked(checked)
        .indeterminate(partial)
        .disabled(disabled)
        .flex()
        .items_center()
        .gap_2()
        .border_1()
        .border_color(cx.theme().transparent)
        .rounded_sm()
        .focus(move |style| style.border_color(ring))
        .when(disabled, |control| control.opacity(0.5))
        .child(
            div()
                .size(px(16.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .rounded_sm()
                .border_color(if checked || partial {
                    cx.theme().primary
                } else {
                    cx.theme().input
                })
                .bg(if checked || partial {
                    cx.theme().primary
                } else {
                    cx.theme().background
                })
                .child(
                    Icon::new(if partial {
                        gpui_kit::assets::IconName::Minus
                    } else {
                        gpui_kit::assets::IconName::Check
                    })
                    .size(px(12.))
                    .text_color(cx.theme().primary_foreground)
                    .opacity(if checked || partial { 1. } else { 0. }),
                ),
        )
}

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
pub(super) struct ChooseTheme {
    pub preference: ThemePreference,
}

pub(super) fn apply_preference(
    preference: ThemePreference,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    cx.set_window_appearance(match preference {
        ThemePreference::System => None,
        ThemePreference::Light => Some(WindowAppearance::Light),
        ThemePreference::Dark => Some(WindowAppearance::Dark),
    });
    let appearance = window
        .as_ref()
        .map(|window| window.appearance())
        .unwrap_or_else(|| cx.window_appearance());
    apply_theme(
        preference.is_dark(matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        )),
        window,
        cx,
    );
}

pub(super) fn apply_theme(dark: bool, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    let theme = Theme::global_mut(cx);
    theme.focus_ring = true;
    theme.scrollbar_mode = gpui_kit::component::scroll::ScrollbarMode::Always;
    let colors = &mut theme.colors;
    let muted = colors.muted;
    for color in [
        &mut colors.muted_foreground,
        &mut colors.success,
        &mut colors.warning,
        &mut colors.danger,
        &mut colors.info,
    ] {
        *color = readable_color(*color, &[muted]);
    }
    // Keep the stock visible tint while making contrast independent of the parent surface.
    colors.button = colors.background.blend(colors.button);
    colors.button_hover = colors.background.blend(colors.button_hover);
    colors.button_active = colors.background.blend(colors.button_active);
    colors.button_foreground = readable_color(
        colors.button_foreground,
        &[colors.button, colors.button_hover, colors.button_active],
    );
    colors.button_primary = colors.background.blend(colors.button_primary);
    colors.button_primary_hover = colors.background.blend(colors.button_primary_hover);
    colors.button_primary_active = colors.background.blend(colors.button_primary_active);
    colors.button_primary_foreground = readable_color(
        colors.button_primary_foreground,
        &[
            colors.button_primary,
            colors.button_primary_hover,
            colors.button_primary_active,
        ],
    );
    colors.button_danger = colors.background.blend(colors.button_danger);
    colors.button_danger_hover = colors.background.blend(colors.button_danger_hover);
    colors.button_danger_active = colors.background.blend(colors.button_danger_active);
    colors.button_danger_foreground = readable_color(
        colors.button_danger_foreground,
        &[
            colors.button_danger,
            colors.button_danger_hover,
            colors.button_danger_active,
        ],
    );
    theme.tokens = (&theme.colors).into();
    Theme::sync_base(cx);
    if let Some(window) = window {
        window.refresh();
    }
}

fn luminance(color: Hsla) -> f64 {
    let color = color.to_rgb();
    let linear = |channel: f32| {
        let channel = f64::from(channel);
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

pub(super) fn contrast_ratio(foreground: Hsla, background: Hsla) -> f64 {
    let foreground = luminance(background.blend(foreground));
    let background = luminance(background);
    (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
}

fn readable_color(color: Hsla, backgrounds: &[Hsla]) -> Hsla {
    let contrast = |color: Hsla| {
        backgrounds
            .iter()
            .map(|background| contrast_ratio(color, *background))
            .fold(f64::INFINITY, f64::min)
    };
    if contrast(color) >= 4.5 {
        return color;
    }
    let mut corrected = color;
    corrected.l = 0.;
    let black_contrast = contrast(corrected);
    corrected.l = 1.;
    let target = if black_contrast > contrast(corrected) {
        0.
    } else {
        1.
    };
    let (mut low, mut high) = (0., 1.);
    // Preserve hue/saturation and minimize the correction, with a small rendering margin.
    for _ in 0..24 {
        let fraction = (low + high) / 2.;
        corrected.l = color.l + (target - color.l) * fraction;
        if contrast(corrected) >= 4.6 {
            high = fraction;
        } else {
            low = fraction;
        }
    }
    corrected.l = color.l + (target - color.l) * high;
    corrected
}

pub(super) fn group(key: &str) -> (usize, &'static str) {
    match key {
        "label" | "url" | "id" | "displayName" | "description" => (0, "General"),
        "viewportNames"
        | "viewportsPath"
        | "selectors"
        | "misMatchThreshold"
        | "pixelMatchThreshold"
        | "imageSimplification"
        | "imageSimplificationBlockSize"
        | "imageSimplificationMinSize"
        | "imageSimplificationExcludeSelectors"
        | "delay"
        | "postInteractionWait"
        | "width"
        | "height" => (1, "Capture"),
        "needs" | "state" | "restore" | "requiredLogin" | "cookiePath" => {
            (2, "Session & dependencies")
        }
        "browser"
        | "asyncCaptureLimit"
        | "asyncCompareLimit"
        | "autoCleanRuns"
        | "keepTestRuns"
        | "debug"
        | "elementTimeout"
        | "actionabilityTimeout"
        | "settleTimeout"
        | "scenarioTimeout" => (3, "Execution"),
        "hideSelectors" | "removeSelectors" | "cssOverridePath" | "jsOnReadyPath"
        | "useCssOverride" | "urlReplacements" => (4, "Page setup"),
        _ => (5, "Behavior"),
    }
}

impl Workbench {
    pub(super) fn change_theme(
        &mut self,
        action: &ChooseTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        if self.busy || self.preferences.theme_preference() == action.preference {
            return;
        }
        self.preferences.set_theme_preference(action.preference);
        apply_preference(action.preference, Some(window), cx);
        self.save_preferences(window, cx);
    }

    pub(super) fn system_appearance_changed(
        &mut self,
        appearance: WindowAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preferences.theme_preference() == ThemePreference::System {
            apply_theme(
                matches!(
                    appearance,
                    WindowAppearance::Dark | WindowAppearance::VibrantDark
                ),
                Some(window),
                cx,
            );
            cx.notify();
        }
    }

    pub(super) fn render_theme_picker(&self) -> AnyElement {
        let preference = self.preferences.theme_preference();
        let (label, icon) = match preference {
            ThemePreference::System => ("System", gpui_kit::assets::IconName::Monitor),
            ThemePreference::Light => ("Light", gpui_kit::assets::IconName::Sun),
            ThemePreference::Dark => ("Dark", gpui_kit::assets::IconName::Moon),
        };
        let focus = self.focus.clone();
        Button::new("theme-picker")
            .ghost()
            .icon(icon)
            .accessibility_label(format!("Appearance: {label}"))
            .tooltip(format!("Appearance: {label}"))
            .disabled(self.busy)
            .dropdown_menu(move |menu, window, cx| {
                restore_dropdown_menu_focus(menu.focus_handle(cx), focus.clone(), window, cx);
                let mut menu = menu.action_context(focus.clone());
                for (choice, label) in [
                    (ThemePreference::System, "System"),
                    (ThemePreference::Light, "Light"),
                    (ThemePreference::Dark, "Dark"),
                ] {
                    menu = menu.menu_with_check(
                        label,
                        preference == choice,
                        Box::new(ChooseTheme { preference: choice }),
                    );
                }
                menu
            })
            .into_any_element()
    }
}

#[cfg(all(test, feature = "gui-tests"))]
mod tests {
    use gpui_kit::TestAppContext;
    use gpui_kit::component::ActiveTheme;

    #[gpui_kit::test]
    fn both_palettes_keep_text_readable_and_keyboard_focus_visible(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut failures = Vec::new();
        for dark in [false, true] {
            cx.update(|cx| {
                super::apply_theme(dark, None, cx);
                let theme = cx.theme();
                for (name, foreground, background) in [
                    ("body", theme.foreground, theme.background),
                    ("secondary", theme.muted_foreground, theme.muted),
                    ("selection", theme.accent_foreground, theme.accent),
                    (
                        "primary button",
                        theme.button_primary_foreground,
                        theme.button_primary,
                    ),
                    (
                        "primary button hover",
                        theme.button_primary_foreground,
                        theme.button_primary_hover,
                    ),
                    (
                        "primary button active",
                        theme.button_primary_foreground,
                        theme.button_primary_active,
                    ),
                    ("neutral button", theme.button_foreground, theme.button),
                    (
                        "neutral button hover",
                        theme.button_foreground,
                        theme.button_hover,
                    ),
                    (
                        "neutral button active",
                        theme.button_foreground,
                        theme.button_active,
                    ),
                    (
                        "danger button",
                        theme.button_danger_foreground,
                        theme.button_danger,
                    ),
                    (
                        "danger button hover",
                        theme.button_danger_foreground,
                        theme.button_danger_hover,
                    ),
                    (
                        "danger button active",
                        theme.button_danger_foreground,
                        theme.button_danger_active,
                    ),
                    ("popup", theme.popover_foreground, theme.popover),
                    ("tab", theme.tab_foreground, theme.tab_bar_segmented),
                    ("success", theme.success, theme.muted),
                    ("warning", theme.warning, theme.muted),
                    ("error", theme.danger, theme.muted),
                    ("info", theme.info, theme.muted),
                ] {
                    for (surface_name, surface) in [
                        ("background", theme.background),
                        ("muted", theme.muted),
                        ("primary", theme.primary),
                    ] {
                        let background = surface.blend(background);
                        let contrast = super::contrast_ratio(foreground, background);
                        let readable = contrast >= 4.5;
                        if !readable {
                            failures.push(format!(
                                "{name} on {surface_name} contrast {contrast:.2} with dark={dark}: {foreground:?} on {background:?}"
                            ));
                        }
                    }
                }
                assert!(theme.focus_ring, "Keyboard focus must stay visible");
            });
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
