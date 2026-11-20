use super::*;

#[derive(Clone, Copy, PartialEq, serde::Deserialize, schemars::JsonSchema)]
pub(super) enum Destination {
    Previous,
    Next,
    First,
    Last,
}

#[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, Action)]
pub(super) struct MoveListSelection {
    pub destination: Destination,
    pub extend: bool,
    pub additive: bool,
}

impl MoveListSelection {
    pub fn modifiers(&self) -> Modifiers {
        Modifiers {
            shift: self.extend,
            control: self.additive,
            ..Default::default()
        }
    }

    pub fn focus_only(&self) -> bool {
        self.additive && !self.extend
    }

    pub fn index(&self, current: Option<usize>, count: usize) -> Option<usize> {
        let last = count.checked_sub(1)?;
        Some(match self.destination {
            Destination::Previous => current.map_or(last, |index| index.saturating_sub(1)),
            Destination::Next => current.map_or(0, |index| (index + 1).min(last)),
            Destination::First => 0,
            Destination::Last => last,
        })
    }
}

pub(super) struct ListState<Item> {
    pub focus: FocusHandle,
    pub scroll: ScrollHandle,
    pub current: Option<Item>,
}

impl<Item: Clone + PartialEq> ListState<Item> {
    pub fn new(cx: &mut App) -> Self {
        Self {
            focus: cx.focus_handle().tab_index(0),
            scroll: ScrollHandle::new(),
            current: None,
        }
    }

    pub fn focus_item(&mut self, item: Item, window: &mut Window, cx: &mut App) {
        self.current = Some(item);
        window.focus(&self.focus, cx);
    }

    pub fn current_item(&self, visible: &[Item], fallback: Option<&Item>) -> Option<Item> {
        self.current
            .as_ref()
            .filter(|item| visible.contains(item))
            .or_else(|| fallback.filter(|item| visible.contains(item)))
            .or_else(|| visible.first())
            .cloned()
    }

    pub fn move_to(
        &mut self,
        action: &MoveListSelection,
        visible: &[Item],
        fallback: Option<&Item>,
    ) -> Option<Item> {
        let current = self
            .current
            .as_ref()
            .and_then(|item| visible.iter().position(|candidate| candidate == item))
            .or_else(|| {
                fallback.and_then(|item| visible.iter().position(|candidate| candidate == item))
            });
        let index = action.index(current, visible.len())?;
        let item = visible[index].clone();
        self.current = Some(item.clone());
        // Append scrollbar overlays after rows to keep direct-child indices aligned.
        self.scroll.scroll_to_item(index);
        Some(item)
    }
}

pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            "f2",
            files::FileCommand {
                operation: files::Operation::Rename,
            },
            Some("RegressifySuiteList"),
        ),
        KeyBinding::new(
            "delete",
            files::FileCommand {
                operation: files::Operation::Delete,
            },
            Some("RegressifySuiteList"),
        ),
    ]);
    for context in ["RegressifyList", "RegressifyChecklist"] {
        let bindings = [
            load_binding("secondary-a", SelectAllListItems, context, cx),
            load_binding("secondary-shift-a", ClearListSelection, context, cx),
            KeyBinding::new("escape", ClearListSelection, Some(context)),
        ];
        cx.bind_keys(bindings);
    }
    let toggle = load_binding("secondary-space", ToggleListItem, "RegressifyList", cx);
    cx.bind_keys([
        KeyBinding::new("space", ToggleListItem, Some("RegressifyList")),
        toggle,
        KeyBinding::new("enter", OpenListItem, Some("RegressifyList")),
        KeyBinding::new(
            "enter",
            gpui_kit::base::actions::Confirm { secondary: false },
            Some("RegressifyList > Tree"),
        ),
    ]);
    for (key, destination) in [
        ("up", Destination::Previous),
        ("down", Destination::Next),
        ("home", Destination::First),
        ("end", Destination::Last),
    ] {
        for (prefix, extend, additive) in [
            ("", false, false),
            ("shift-", true, false),
            ("secondary-", false, true),
            ("secondary-shift-", true, true),
        ] {
            let binding = load_binding(
                &format!("{prefix}{key}"),
                MoveListSelection {
                    destination,
                    extend,
                    additive,
                },
                "RegressifyList",
                cx,
            );
            cx.bind_keys([binding]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Destination, MoveListSelection};

    #[test]
    fn list_movement_handles_empty_missing_and_boundary_items() {
        for (destination, missing, first, last) in [
            (Destination::Previous, 2, 0, 1),
            (Destination::Next, 0, 1, 2),
            (Destination::First, 0, 0, 0),
            (Destination::Last, 2, 2, 2),
        ] {
            let action = MoveListSelection {
                destination,
                extend: false,
                additive: false,
            };
            assert_eq!(action.index(None, 0), None);
            assert_eq!(action.index(None, 3), Some(missing));
            assert_eq!(action.index(Some(0), 3), Some(first));
            assert_eq!(action.index(Some(2), 3), Some(last));
            assert_eq!(action.index(Some(0), 1), Some(0));
        }
    }
}
