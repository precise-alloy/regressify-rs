#![allow(dead_code)]
use serde::{Deserialize, Serialize};

use crate::types::Selectors;

/// One browser action per list item. Actions run in order; a failure stops the current scenario.
/// Element selectors accept CSS plus text=, xpath=, :has-text(), and >> chains; hide, remove,
/// and screenshot selectors are standard CSS only.
#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
#[derive(schemars::JsonSchema)]
pub enum TestAction {
    Check(CheckAction),
    Click(ClickAction),
    Focus(FocusAction),
    Goto(GotoAction),
    Hide(HideAction),
    Hover(HoverAction),
    Input(InputAction),
    Persist(PersistAction),
    Press(PressAction),
    Remove(RemoveAction),
    Save(SaveAction),
    Select(SelectAction),
    Scroll(ScrollAction),
    SwitchTab(SwitchTabAction),
    Uncheck(UncheckAction),
    Wait(WaitAction),
    Drag(DragAction),
    DropFiles(DropFilesAction),
    Wheel(WheelAction),
    Navigate(NavigateAction),
    Dialog(DialogAction),
    Key(KeyAction),
    Tap(TapAction),
    Swipe(SwipeAction),
    LongPress(LongPressAction),
    Blur(BlurAction),
    Pinch(PinchAction),
    SelectText(SelectTextAction),
    Paste(PasteAction),
    CloseTab(CloseTabAction),
}

impl TestAction {
    pub(crate) fn frame(&self) -> &Option<Frame> {
        match self {
            Self::Check(action) => &action.frame,
            Self::Click(action) => &action.frame,
            Self::Focus(action) => &action.frame,
            Self::Goto(action) => &action.frame,
            Self::Hide(action) => &action.frame,
            Self::Hover(action) => &action.frame,
            Self::Input(action) => &action.frame,
            Self::Persist(action) => &action.frame,
            Self::Press(action) => &action.frame,
            Self::Remove(action) => &action.frame,
            Self::Save(action) => &action.frame,
            Self::Select(action) => &action.frame,
            Self::Scroll(action) => &action.frame,
            Self::SwitchTab(_) | Self::CloseTab(_) => &None,
            Self::Uncheck(action) => &action.frame,
            Self::Wait(action) => &action.frame,
            Self::Drag(action) => &action.frame,
            Self::DropFiles(action) => &action.frame,
            Self::Wheel(action) => &action.frame,
            Self::Navigate(_) | Self::Dialog(_) => &None,
            Self::Key(action) => &action.frame,
            Self::Tap(action) => &action.frame,
            Self::Swipe(action) => &action.frame,
            Self::LongPress(action) => &action.frame,
            Self::Blur(action) => &action.frame,
            Self::Pinch(action) => &action.frame,
            Self::SelectText(action) => &action.frame,
            Self::Paste(action) => &action.frame,
        }
    }

    pub(crate) fn viewport_names(&self) -> Option<&[String]> {
        let names = match self {
            Self::Check(action) => &action.viewport_names,
            Self::Click(action) => &action.viewport_names,
            Self::Focus(action) => &action.viewport_names,
            Self::Goto(action) => &action.viewport_names,
            Self::Hide(action) => &action.viewport_names,
            Self::Hover(action) => &action.viewport_names,
            Self::Input(action) => &action.viewport_names,
            Self::Persist(action) => &action.viewport_names,
            Self::Press(action) => &action.viewport_names,
            Self::Remove(action) => &action.viewport_names,
            Self::Save(action) => &action.viewport_names,
            Self::Select(action) => &action.viewport_names,
            Self::Scroll(action) => &action.viewport_names,
            Self::SwitchTab(action) => &action.viewport_names,
            Self::Uncheck(action) => &action.viewport_names,
            Self::Wait(action) => &action.viewport_names,
            Self::Drag(action) => &action.viewport_names,
            Self::DropFiles(action) => &action.viewport_names,
            Self::Wheel(action) => &action.viewport_names,
            Self::Navigate(action) => &action.viewport_names,
            Self::Dialog(action) => &action.viewport_names,
            Self::Key(action) => &action.viewport_names,
            Self::Tap(action) => &action.viewport_names,
            Self::Swipe(action) => &action.viewport_names,
            Self::LongPress(action) => &action.viewport_names,
            Self::Blur(action) => &action.viewport_names,
            Self::Pinch(action) => &action.viewport_names,
            Self::SelectText(action) => &action.viewport_names,
            Self::Paste(action) => &action.viewport_names,
            Self::CloseTab(action) => &action.viewport_names,
        };
        names.as_ref().map(Selectors::values)
    }

    pub(crate) fn uses_touch(&self) -> bool {
        matches!(
            self,
            Self::Tap(_) | Self::Swipe(_) | Self::LongPress(_) | Self::Pinch(_)
        )
    }

    pub(crate) fn can_trigger_dialog(&self) -> bool {
        matches!(
            self,
            Self::Click(_)
                | Self::Check(_)
                | Self::Uncheck(_)
                | Self::Input(_)
                | Self::Press(_)
                | Self::Key(_)
                | Self::Select(_)
                | Self::Goto(_)
                | Self::Navigate(_)
                | Self::Tap(_)
                | Self::LongPress(_)
                | Self::Drag(_)
                | Self::DropFiles(_)
                | Self::Swipe(_)
                | Self::Wheel(_)
                | Self::Focus(_)
                | Self::Hover(_)
                | Self::Blur(_)
                | Self::Pinch(_)
                | Self::SelectText(_)
                | Self::Paste(_)
                | Self::CloseTab(_)
        )
    }
}

/// Optional iframe selector, or a list of selectors from outermost to innermost frame.
/// Same-origin and cross-origin frames work; each selector must match an iframe or frame
/// element. Omit to target the main page. Quote selectors beginning with # in YAML.
#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
#[derive(schemars::JsonSchema)]
#[schemars(example = &"iframe#checkout", example = vec!["iframe.outer", "iframe.inner"])]
pub enum Frame {
    Single(String),
    Multiple(Vec<String>),
}

/// File input: a single file path or an array of file paths (relative to visual_tests).
#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
#[derive(schemars::JsonSchema)]
#[schemars(example = "uploads/sample.pdf", example = vec!["uploads/first.pdf", "uploads/second.pdf"])]
pub enum FileInput {
    Single(String),
    Multiple(Vec<String>),
}

/// Option value, or a list of values for a select element with the multiple attribute.
#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
#[derive(schemars::JsonSchema)]
#[schemars(example = &"option", example = vec!["first", "second"])]
pub enum SelectValue {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct CheckAction {
    /// Selector of a native checkbox or radio button. Clicks only when not already checked; waits for visibility and an unobscured, enabled target. Use click instead for custom role="checkbox" or role="switch" controls or when the native input is visually hidden.
    #[schemars(example = "#accept-terms")]
    pub check: String,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct ClickAction {
    /// Element selector to click. Scrolls into view and waits until visible, stable, enabled, and not covered by another element. Navigation that starts immediately is awaited; add a wait for later or client-side changes.
    #[schemars(example = "#submit")]
    pub click: String,
    /// Mouse button, default left. Right clicks open page context menus; middle clicks retain normal browser behavior.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<PointerButton>,
    /// Number of consecutive clicks: 1 (default), 2 for double-click, or 3 for triple-click.
    #[schemars(range(min = 1, max = 3))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click_count: Option<u8>,
    /// Keyboard modifiers held for this click and released afterwards.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifiers: Option<Vec<PointerModifier>>,
    /// CSS-pixel position inside the element's border box; omitted means its center.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<ActionPoint>,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct FocusAction {
    /// Selector of the element to focus. Scrolls into view, checks that it is interactable, and fails unless it becomes the active element.
    #[schemars(example = "#email")]
    pub focus: String,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct GotoAction {
    /// Navigate the page (or frame) to this URL and wait until the document is interactive. Test captures apply URL replacements; reference captures apply only rules with find (find becomes ref) and otherwise use it as written.
    #[schemars(example = "https://example.com")]
    pub goto: String,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct HideAction {
    /// Standard CSS selector of elements to hide with visibility: hidden in the current document, keeping their layout space. Waits up to elementTimeout for a match and fails if none appears; use hideSelectors for optional elements.
    #[schemars(example = ".timestamp")]
    pub hide: String,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct HoverAction {
    /// Element selector to hover using the real mouse pointer. Scrolls into view and waits for an interactable target.
    #[schemars(example = "#navigation-menu")]
    pub hover: String,
    /// CSS-pixel position inside the element's border box; omitted means its center.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<ActionPoint>,
    /// Keyboard modifiers held during this hover and released afterwards.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifiers: Option<Vec<PointerModifier>>,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct RemoveAction {
    /// Standard CSS selector of elements to hide with display: none in the current document; they stay in the DOM but take no layout space. Waits up to elementTimeout for a match and fails if none appears; use removeSelectors for optional elements.
    #[schemars(example = ".advertisement")]
    pub remove: String,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct InputAction {
    /// Selector of the editable input, textarea, contenteditable element, or native value input (range, color, date, time, datetime-local, month, week). Supply exactly one of value or file; neither or both fails during preflight before any browser starts.
    #[schemars(example = "#email")]
    pub input: String,
    /// Literal text typed with keyboard events after the existing text is selected and deleted (unless append is true), or the value assigned to a native range/color/date/time input. No environment variables are substituted. Quote numbers, true, and false when they are text. Use an empty quoted string to clear a text field.
    #[schemars(example = "qa@example.com", example = &"")]
    pub value: Option<String>,
    /// Append to existing text instead of clearing it first (default: false). Only supported for text inputs, textareas, and contenteditable targets.
    pub append: Option<bool>,
    /// File path or list of file paths, relative to visual_tests/. Without useFileChooser the target must be a visible input type=file; do not put file contents here.
    pub file: Option<FileInput>,
    /// Click the target (for example a visible upload button) to open a file chooser, then supply file (default: false).
    pub use_file_chooser: Option<bool>,
    /// Fixed milliseconds between typed characters, from 0 (default) to 10000. Only valid with value on text-typing targets, not file or range/color/date controls.
    #[schemars(range(max = 10000))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typing_delay: Option<u64>,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct PersistAction {
    /// State name without .json. Writes cookies and origin-scoped localStorage to the output directory's states/<name>.json; load it with state or restore in a later run. The file holds session secrets.
    #[schemars(example = "signed-in")]
    pub persist: String,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct PressAction {
    /// Selector of the element that receives the key. The key property is also required.
    #[schemars(example = "#search")]
    pub press: String,
    /// Case-sensitive key name or combination with one non-modifier key: Enter, Tab, Escape, ArrowDown, Ctrl+A, Shift+Tab, or ControlOrMeta+A, for example. Unknown or wrongly cased keys fail during preflight before any browser starts.
    #[schemars(example = &"Enter", example = &"Tab", example = "Ctrl+A", example = "Shift+Tab")]
    pub key: String,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct SaveAction {
    /// Checkpoint name saved as <scenario-id>[_<viewport>]__$<name>.png: at most 100 bytes, no path separators or __, and not ending in _diff. A last applicable save among the scenario's own actions replaces the automatic final screenshot, but explicit scenario selectors are still captured.
    #[schemars(example = "after-login")]
    pub save: String,
    /// Standard CSS selector to capture instead of the whole page, or "viewport" for the visible viewport. An element capture uses the first match in the selected tab's main document (or the target frame when frame is set on save), which must be visible, clipped to its box, so open menus or popups extending outside it are cut off. Independent of the scenario's final selector.
    #[schemars(
        length(min = 1),
        regex(pattern = r"\S"),
        example = "#confirmation",
        example = &"viewport"
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct ScrollAction {
    /// Element selector to scroll into view, or the scroll container to scroll when position or offset is set. Does not click or focus the element.
    #[schemars(example = "#footer")]
    pub scroll: String,
    /// Exact scrollLeft/scrollTop position in nonnegative CSS pixels on the target scroll container. Mutually exclusive with offset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<ActionPoint>,
    /// Signed CSS-pixel scrollBy delta on the target scroll container; at least one coordinate must be nonzero. Mutually exclusive with position.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<ActionPoint>,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct SelectAction {
    /// Selector of a native select element. Supply exactly one of value (option values) or label (visible option text); neither or both fails during preflight before any browser starts. For custom dropdowns or comboboxes that are not native <select> elements, use click actions to open the menu and pick the option.
    #[schemars(example = "#country")]
    pub select: String,
    /// Option value, or list of values for a multiple select. Retries while options are missing or disabled, then fails after elementTimeout.
    pub value: Option<SelectValue>,
    /// Visible option text to select instead of matching by value.
    #[schemars(example = &"Sweden")]
    pub label: Option<String>,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct UncheckAction {
    /// Selector of a checked native checkbox to clear. Does nothing when already unchecked; cannot clear a selected radio button. Use click instead for custom role="checkbox" or role="switch" controls or when the native input is visually hidden.
    #[schemars(example = "#newsletter")]
    pub uncheck: String,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct SwitchTabAction {
    /// Select a new tab opened by the current page, the previously selected tab, or the scenario's original tab. Subsequent actions and screenshots use the selected tab.
    pub switch_tab: TabTarget,
    /// Optional URL or glob for a new tab. Waits for its navigation to match; * stays within one path segment and ** matches across segments. Only valid with switchTab: new; test captures apply URL replacements, and reference captures apply rules with find.
    #[schemars(example = "**/quote")]
    pub url: Option<String>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[derive(schemars::JsonSchema)]
pub enum TabTarget {
    New,
    Previous,
    Original,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[derive(schemars::JsonSchema)]
pub struct WaitAction {
    /// Whole milliseconds to pause, or an element selector that must exist and become visible within elementTimeout. With url, the URL is matched first; this pause or selector wait runs afterwards.
    #[schemars(example = 1000, example = "#ready")]
    pub wait: Wait,
    /// URL or glob the page or frame URL must match within elementTimeout before the wait continues. * matches within one path segment, ** across segments, {a,b} lists alternatives, and ? is literal. Test captures apply URL replacements, and reference captures apply rules with find.
    #[schemars(example = "**/dashboard")]
    pub url: Option<String>,
    /// Element condition, default visible. Hidden also matches an absent element; detached requires absence. Only valid with a selector wait.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<WaitState>,
    /// Exact visible text after trimming and collapsing whitespace. Combined with the element state; mutually exclusive with containsText.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Visible text substring after trimming and collapsing whitespace. Combined with the element state; mutually exclusive with text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains_text: Option<String>,
    /// Exact string value of an input, textarea, or select. Combined with the element state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
#[derive(schemars::JsonSchema)]
pub enum Wait {
    Number(u64),

    String(String),
}

/// CSS pixels relative to an element's top-left border box, or a signed gesture offset.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionPoint {
    /// Horizontal CSS pixels. Positions must be nonnegative; offsets may be negative.
    pub x: f64,
    /// Vertical CSS pixels. Positions must be nonnegative; offsets may be negative.
    pub y: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PointerButton {
    #[default]
    Left,
    Middle,
    Right,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, schemars::JsonSchema)]
pub enum PointerModifier {
    Alt,
    Control,
    Meta,
    Shift,
    ControlOrMeta,
}

#[derive(
    Debug, Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum DragMode {
    #[default]
    Auto,
    Pointer,
    Html5,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DragAction {
    /// Source element to drag within the selected document. Auto uses native HTML5 dragging for draggable elements, pointer dragging otherwise.
    pub drag: String,
    /// Destination element in the same document. Supply exactly one of to or offset.
    pub to: Option<String>,
    /// Signed CSS-pixel displacement from the start position, instead of a destination element.
    pub offset: Option<ActionPoint>,
    /// Start position inside the source border box; default center.
    pub position: Option<ActionPoint>,
    /// End position inside the destination border box; only valid with to. Default center.
    pub to_position: Option<ActionPoint>,
    /// Override automatic pointer versus HTML5 drag selection.
    pub mode: Option<DragMode>,
    /// Fixed gesture duration in milliseconds, 1..60000; default 300. Both endpoints must be visible together.
    #[schemars(range(min = 1, max = 60000))]
    pub duration: Option<u64>,
    /// Keyboard modifiers held during the drag and released afterwards.
    pub modifiers: Option<Vec<PointerModifier>>,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DropFilesAction {
    /// Drop-zone element receiving native browser drag-enter, drag-over, and drop events.
    pub drop_files: String,
    /// One or more existing regular files relative to visual_tests. Paths must remain inside that directory.
    pub file: FileInput,
    /// CSS-pixel drop position inside the border box; default center.
    pub position: Option<ActionPoint>,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WheelAction {
    /// Element under the wheel pointer, such as a scroll container or canvas. This sends wheel input, not scrollIntoView alone.
    pub wheel: String,
    /// Horizontal wheel delta in CSS pixels, default zero. At least one delta must be nonzero.
    pub delta_x: Option<f64>,
    /// Vertical wheel delta in CSS pixels, default zero. Positive values scroll down.
    pub delta_y: Option<f64>,
    /// CSS-pixel pointer position inside the border box; default center.
    pub position: Option<ActionPoint>,
    /// Keyboard modifiers held for this wheel event and released afterwards.
    pub modifiers: Option<Vec<PointerModifier>>,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NavigationOperation {
    Back,
    Forward,
    Reload,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NavigateAction {
    /// Use the selected tab's real history or reload it. Missing back/forward entries fail; frame is not supported.
    pub navigate: NavigationOperation,
    /// Bypass the browser cache on reload (default false). Only valid with navigate: reload.
    pub ignore_cache: Option<bool>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DialogResponse {
    Accept,
    Dismiss,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
    BeforeUnload,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DialogAction {
    /// Arm a one-shot JavaScript dialog expectation for the next applicable triggering action in this tab. Missing, extra, or mismatched dialogs fail.
    pub dialog: DialogResponse,
    /// Expected dialog kind; omitted accepts any supported kind. beforeunload requires prior browser user activation.
    pub kind: Option<DialogKind>,
    /// Optional exact expected dialog message. Message contents are never written to diagnostic archives.
    pub message: Option<String>,
    /// Text supplied when accepting a prompt. Requires kind: prompt and dialog: accept; never logged.
    pub prompt_text: Option<String>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyAction {
    /// Send a key or shortcut, for example Tab or ControlOrMeta+A, to the current focus without changing it. A selected frame must already own focus.
    pub key: String,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TapAction {
    /// Element to tap with real touch input. Applicable touch actions enable touch capability before the capture's initial navigation.
    pub tap: String,
    /// CSS-pixel touch position inside the border box; default center.
    pub position: Option<ActionPoint>,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SwipeAction {
    /// Element where a one-finger swipe starts. Browser touch-action and scrolling behavior remain in effect.
    pub swipe: String,
    /// Signed CSS-pixel displacement from the start. The complete gesture must stay inside the visible selected frame and top-level viewport.
    pub offset: ActionPoint,
    /// CSS-pixel start position inside the border box; default center.
    pub position: Option<ActionPoint>,
    /// Fixed gesture duration in milliseconds, 1..60000; default 300.
    #[schemars(range(min = 1, max = 60000))]
    pub duration: Option<u64>,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LongPressAction {
    /// Element to hold with one finger, then release. This sends touch input, not a synthetic contextmenu event.
    pub long_press: String,
    /// CSS-pixel touch position inside the border box; default center.
    pub position: Option<ActionPoint>,
    /// Fixed hold duration in milliseconds, 1..60000; default 600.
    #[schemars(range(min = 1, max = 60000))]
    pub duration: Option<u64>,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlurAction {
    /// Element selector to unfocus, or "active" to unfocus whichever element currently owns focus in the target document. Fails if the target does not currently own focus.
    #[schemars(example = "#email", example = &"active")]
    pub blur: String,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PinchAction {
    /// Element where a two-finger touch pinch or spread gesture is centered. Applicable touch actions enable touch capability before the capture's initial navigation.
    #[schemars(example = "#map-canvas")]
    pub pinch: String,
    /// Two-finger scale factor from 0.1 to 10.0 (excluding 1.0). Values below 1.0 pinch fingers together (zoom out); values above 1.0 spread fingers apart (zoom in).
    #[schemars(range(min = 0.1, max = 10.0), example = 2.0, example = 0.5)]
    pub scale: f64,
    /// CSS-pixel center position inside the border box; default center.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<ActionPoint>,
    /// Fixed gesture duration in milliseconds, 1..60000; default 300.
    #[schemars(range(min = 1, max = 60000))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, Default, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectTextAction {
    /// Selector of the input, textarea, contenteditable, or DOM element whose text should be selected. Omitting text, start, and end selects all text in the element.
    #[schemars(example = "#editor")]
    pub select_text: String,
    /// Exact non-empty substring to select inside the target's text or value. Mutually exclusive with start and end.
    #[schemars(example = "highlighted phrase")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Zero-based inclusive start character offset (Unicode scalar index). Mutually exclusive with text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<usize>,
    /// Zero-based exclusive end character offset (Unicode scalar index, must be greater than start). Mutually exclusive with text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<usize>,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Serialize, Deserialize, Clone, Default, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PasteAction {
    /// Selector of the editable input, textarea, contenteditable, or focusable target receiving pasted text.
    #[schemars(example = "#otp-code")]
    pub paste: String,
    /// Literal text supplied via a cancelable ClipboardEvent('paste') and inserted with CDP if not preventDefault()ed. Never written to diagnostic archives.
    #[schemars(example = "123456")]
    pub value: String,
    /// Insert at the current selection instead of clearing existing text first (default: false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append: Option<bool>,
    /// Optional iframe selector or outermost-to-innermost frame chain.
    pub frame: Option<Frame>,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CloseTabTarget {
    Current,
}

#[derive(Serialize, Deserialize, Clone, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloseTabAction {
    /// Close the currently selected popup tab and return selection to the previously active tab. Closing the scenario's original tab is not allowed; frame is not supported.
    pub close_tab: CloseTabTarget,
    /// Restrict this action to one viewport label or a list of labels (case-insensitive).
    /// Omitted, null, or an empty list means all scenario viewports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport_names: Option<Selectors>,
}

#[derive(
    Debug, Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum WaitState {
    #[default]
    Visible,
    Hidden,
    Attached,
    Detached,
    Enabled,
    Disabled,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action_shapes() -> [serde_json::Value; 30] {
        use serde_json::json;

        [
            json!({ "check": "#accept" }),
            json!({ "click": "#submit" }),
            json!({ "focus": "#email" }),
            json!({ "goto": "https://example.com" }),
            json!({ "hide": ".timestamp" }),
            json!({ "hover": "#menu" }),
            json!({ "input": "#email", "value": "test" }),
            json!({ "persist": "signed-in" }),
            json!({ "press": "#search", "key": "Enter" }),
            json!({ "remove": ".advertisement" }),
            json!({ "save": "after-login", "selector": "#confirmation" }),
            json!({ "select": "#country", "value": "se" }),
            json!({ "scroll": "#footer" }),
            json!({ "switchTab": "new", "url": "**/quote" }),
            json!({ "uncheck": "#newsletter" }),
            json!({ "wait": 100 }),
            json!({ "drag": "#card", "to": "#column" }),
            json!({ "dropFiles": "#upload", "file": "uploads/sample.pdf" }),
            json!({ "wheel": "#list", "deltaY": 200 }),
            json!({ "navigate": "back" }),
            json!({ "dialog": "accept", "kind": "confirm" }),
            json!({ "key": "Tab" }),
            json!({ "tap": "#button" }),
            json!({ "swipe": "#carousel", "offset": { "x": -100, "y": 0 } }),
            json!({ "longPress": "#button" }),
            json!({ "blur": "#email" }),
            json!({ "pinch": "#map", "scale": 2.0 }),
            json!({ "selectText": "#editor", "text": "hello" }),
            json!({ "paste": "#otp", "value": "123456" }),
            json!({ "closeTab": "current" }),
        ]
    }

    #[test]
    fn save_screenshot_selectors_round_trip_yaml_and_json() {
        use serde_json::{Value, json};

        for selector in [
            None,
            Some(Value::Null),
            Some(json!("[data-name=\"it's a card\"]")),
        ] {
            let mut source = json!({ "save": "detail" });
            if let Some(selector) = &selector {
                source["selector"] = selector.clone();
            }
            let yaml = serde_yaml::to_string(&source).unwrap();
            for action in [
                serde_json::from_value::<TestAction>(source.clone()).unwrap(),
                serde_yaml::from_str::<TestAction>(&yaml).unwrap(),
            ] {
                let TestAction::Save(save) = &action else {
                    panic!("Expected a save action");
                };
                assert_eq!(save.selector.as_deref(), source["selector"].as_str());
                let serialized = serde_json::to_value(&action).unwrap();
                assert_eq!(serialized["save"], "detail");
                assert_eq!(serialized["selector"], source["selector"]);
                assert_eq!(
                    serialized.get("selector").is_some(),
                    save.selector.is_some()
                );
            }
        }
        for selector in [json!(12), json!(false), json!(["#content"]), json!({})] {
            let source = json!({ "save": "detail", "selector": selector });
            assert!(serde_json::from_value::<TestAction>(source.clone()).is_err());
            assert!(
                serde_yaml::from_str::<TestAction>(&serde_yaml::to_string(&source).unwrap())
                    .is_err()
            );
        }
    }

    #[test]
    fn all_action_viewport_names_round_trip_yaml_and_json() {
        use serde_json::{Value, json};

        for shape in action_shapes() {
            for filter in [
                None,
                Some(Value::Null),
                Some(json!([])),
                Some(json!("mobile")),
                Some(json!(["Desktop", "mobile", "mobile"])),
            ] {
                let mut source = shape.clone();
                let unfiltered: TestAction = serde_json::from_value(shape.clone()).unwrap();
                let mut expected = serde_json::to_value(unfiltered).unwrap();
                if let Some(filter) = &filter {
                    source["viewportNames"] = filter.clone();
                    if !filter.is_null() {
                        expected["viewportNames"] = filter.clone();
                    }
                }
                let expected_filter: Option<Vec<String>> = match filter.unwrap_or(Value::Null) {
                    Value::String(name) => Some(vec![name]),
                    filter => serde_json::from_value(filter).unwrap(),
                };
                let yaml = serde_yaml::to_string(&source).unwrap();
                for action in [
                    serde_json::from_value::<TestAction>(source.clone()).unwrap(),
                    serde_yaml::from_str::<TestAction>(&yaml).unwrap(),
                ] {
                    assert_eq!(
                        action.viewport_names(),
                        expected_filter.as_deref(),
                        "{source}"
                    );
                    assert_eq!(serde_json::to_value(&action).unwrap(), expected, "{source}");
                    let round_trip: TestAction =
                        serde_yaml::from_str(&serde_yaml::to_string(&action).unwrap()).unwrap();
                    assert_eq!(serde_json::to_value(round_trip).unwrap(), expected);
                }
            }
        }
    }

    #[test]
    fn all_action_viewport_names_reject_malformed_and_mixed_shapes() {
        use serde_json::json;

        for shape in action_shapes() {
            for filter in [
                json!(12),
                json!(false),
                json!([12]),
                json!([null]),
                json!([false]),
                json!({ "mobile": true }),
            ] {
                let mut source = shape.clone();
                source["viewportNames"] = filter;
                assert!(serde_json::from_value::<TestAction>(source.clone()).is_err());
                let yaml = serde_yaml::to_string(&source).unwrap();
                assert!(serde_yaml::from_str::<TestAction>(&yaml).is_err(), "{yaml}");
            }
            for property in [
                "unknownProperty",
                if shape.get("click").is_some() {
                    "hide"
                } else {
                    "click"
                },
            ] {
                let mut source = shape.clone();
                source["viewportNames"] = json!(["mobile"]);
                source[property] = json!("#target");
                assert!(serde_json::from_value::<TestAction>(source.clone()).is_err());
                let yaml = serde_yaml::to_string(&source).unwrap();
                assert!(serde_yaml::from_str::<TestAction>(&yaml).is_err(), "{yaml}");
            }
        }
    }

    #[test]
    fn switch_tab_actions_use_explicit_camel_case_targets() {
        for yaml in [
            "switchTab: new\nurl: '**/quote'\n",
            "switchTab: original\n",
            "switchTab: previous\n",
        ] {
            let expected: serde_json::Value = serde_yaml::from_str(yaml).unwrap();
            let action: TestAction =
                serde_yaml::from_str(yaml).expect("valid tab selection action");
            let actual = serde_json::to_value(action).unwrap();
            assert_eq!(actual["switchTab"], expected["switchTab"]);
            assert_eq!(actual["url"], expected["url"]);
        }
    }

    #[test]
    fn switch_tab_actions_reject_unknown_targets_and_properties() {
        for yaml in [
            "switchTab: missing\n",
            "switch_tab: new\n",
            "switchTab: new\nframe: '#frame'\n",
            "switchTab: new\nclick: '#link'\n",
        ] {
            assert!(serde_yaml::from_str::<TestAction>(yaml).is_err(), "{yaml}");
        }
    }

    #[test]
    fn action_structs_reject_unknown_properties() {
        let result = serde_yaml::from_str::<ClickAction>("click: '#submit'\nfram: '#app'\n");

        assert!(matches!(result, Err(error) if error.to_string().contains("unknown field")));
    }
}
