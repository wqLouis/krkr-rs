//! In-engine menu bar / dropdowns drawn with Bevy UI from the native
//! `MenuItem` tree.
//!
//! The Linux/Bevy host has no native OS menu, so this module renders the
//! logical tree exposed by `crates/tvp-natives/src/menu_item.rs`
//! (`menu_snapshots`) as a real in-app menu: a horizontal bar of top-level
//! items, dropdown columns for `popup`/hover-opened submenus, and
//! checked/radio/disabled state plus shortcut text on each row. Activating a
//! leaf calls [`tvp_natives::fire_menu_click`], which runs the script
//! `onClick` handler exactly like the reference's `fireClick`
//! (`reference/cpp/core/visual/MenuItemImpl.cpp:305`).
//!
//! Re-rendering is driven by a signature: whenever the logical tree or the
//! open path changes, the whole menu entity tree is rebuilt.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use bevy::prelude::*;

use tvp_natives::{
    MenuItemSnapshot, MenuSnapshot, fire_menu_click, menu_snapshots, take_popup_request,
};

/// Height of the horizontal bar in logical pixels.
pub const MENU_BAR_HEIGHT: f32 = 26.0;
/// Height of one dropdown row.
pub const MENU_ENTRY_HEIGHT: f32 = 22.0;
/// Minimum width of a dropdown column.
pub const MENU_ENTRY_MIN_WIDTH: f32 = 140.0;
/// Font size used for captions and shortcut text.
const MENU_FONT_SIZE: f32 = 14.0;

/// One flattened, currently-visible menu entry. Pure data so it can be unit
/// tested without a Bevy app.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MenuNode {
    pub handle: i64,
    pub caption: String,
    pub shortcut: String,
    pub checked: bool,
    pub radio: bool,
    pub enabled: bool,
    pub has_children: bool,
    /// `0` for a top-level bar item, `1+` for dropdown rows.
    pub depth: u32,
    /// Parent handle, or `0` for a top-level item.
    pub parent: i64,
}

/// Flatten a window's menu tree into the entries visible for `open`.
///
/// `open` is the path of handles whose dropdowns are expanded (root-most
/// first). Top-level items are always visible; an item's children are visible
/// only while the item itself is in `open`.
pub fn visible_nodes(view: &MenuSnapshot, open: &[i64]) -> Vec<MenuNode> {
    let mut out = Vec::new();
    for child in &view.root.children {
        push_node(child, 0, 0, open, &mut out);
    }
    out
}

fn push_node(
    item: &MenuItemSnapshot,
    depth: u32,
    parent: i64,
    open: &[i64],
    out: &mut Vec<MenuNode>,
) {
    if !item.visible {
        return;
    }
    out.push(MenuNode {
        handle: item.handle,
        caption: item.caption.clone(),
        shortcut: item.shortcut.clone(),
        checked: item.checked,
        radio: item.radio,
        enabled: item.enabled,
        has_children: !item.children.is_empty(),
        depth,
        parent,
    });
    if open.contains(&item.handle) {
        for child in &item.children {
            push_node(child, depth + 1, item.handle, open, out);
        }
    }
}

/// Path of handles from the top level down to `target` (inclusive), or `None`
/// when the item is not in this tree.
pub fn path_to_handle(root: &MenuItemSnapshot, target: i64) -> Option<Vec<i64>> {
    if root.handle == target {
        return Some(vec![root.handle]);
    }
    for child in &root.children {
        if let Some(mut path) = path_to_handle(child, target) {
            path.insert(0, root.handle);
            return Some(path);
        }
    }
    None
}

/// Marker for the spawned menu container.
#[derive(Component)]
pub struct MenuRoot;

/// Component attached to every interactive menu row.
#[derive(Component, Clone, Copy, Debug)]
pub struct MenuEntry {
    pub handle: i64,
    pub depth: u32,
    pub has_children: bool,
    pub enabled: bool,
}

/// Renderer-side state: the currently open dropdown path and the entity of
/// the last built menu, plus the signature used to avoid needless rebuilds.
#[derive(Resource, Default)]
pub struct MenuUiState {
    signature: u64,
    open: Vec<i64>,
    root: Option<Entity>,
}

/// Registers the menu systems. Add after the default plugins.
pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuUiState>()
            .add_systems(Update, (menu_update, menu_sync_ui).chain());
    }
}

/// Apply hover/click interaction and any pending `popup` request to the open
/// path. Runs before [`menu_sync_ui`] so a change is rendered the same frame.
fn menu_update(
    mut state: ResMut<MenuUiState>,
    mut entries: Query<(&Interaction, &MenuEntry, &mut BackgroundColor), Changed<Interaction>>,
) {
    for (interaction, entry, mut background) in &mut entries {
        // Keep the hover highlight in sync even for disabled items.
        *background = BackgroundColor(entry_background(entry.depth, entry.enabled, *interaction));
        if !entry.enabled {
            continue;
        }
        match *interaction {
            Interaction::Hovered => {
                if entry.has_children {
                    open_at(&mut state.open, entry.handle, entry.depth);
                }
            }
            Interaction::Pressed => {
                if entry.has_children {
                    open_at(&mut state.open, entry.handle, entry.depth);
                } else {
                    let handle = entry.handle;
                    state.open.clear();
                    if !fire_menu_click(handle) {
                        log::debug!("menu: handle {handle} did not fire (disabled or gone)");
                    }
                }
            }
            Interaction::None => {}
        }
    }

    if let Some(handle) = take_popup_request()
        && let Some(path) = menu_snapshots()
            .iter()
            .find_map(|view| path_to_handle(&view.root, handle))
    {
        // `path` ends at the requested item; its children open when the item
        // itself is in the open set.
        state.open = path;
    }
}

/// Open `handle` at `depth`, dropping any deeper/sibling menus.
fn open_at(open: &mut Vec<i64>, handle: i64, depth: u32) {
    let depth = depth as usize;
    open.truncate(depth);
    if open.last() != Some(&handle) {
        open.push(handle);
    }
}

/// Rebuild the Bevy UI when the tree or open path changes.
fn menu_sync_ui(
    mut commands: Commands,
    mut state: ResMut<MenuUiState>,
    existing: Query<Entity, With<MenuRoot>>,
) {
    let views = menu_snapshots();
    let mut nodes: Vec<MenuNode> = Vec::new();
    for view in &views {
        nodes.extend(visible_nodes(view, &state.open));
    }
    let signature = signature_of(&nodes, &state.open);

    // The root may have been removed by a previous frame; treat a missing
    // entity as "needs rebuild".
    let root_alive = state.root.is_some_and(|e| existing.contains(e));
    if root_alive && signature == state.signature {
        return;
    }
    if let Some(entity) = state.root.take()
        && existing.contains(entity)
    {
        commands.entity(entity).despawn();
    }
    state.signature = signature;
    if nodes.is_empty() {
        return;
    }

    let root = commands
        .spawn((
            MenuRoot,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Px(MENU_BAR_HEIGHT),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.10, 0.10, 0.12, 0.92)),
            ZIndex(1000),
        ))
        .id();

    // Group dropdown children by their parent handle.
    let mut by_parent: HashMap<i64, Vec<&MenuNode>> = HashMap::new();
    for node in &nodes {
        by_parent.entry(node.parent).or_default().push(node);
    }
    if let Some(top) = by_parent.get(&0) {
        for node in top {
            spawn_entry(&mut commands, root, node, &by_parent, &state.open);
        }
    }
    state.root = Some(root);
}

/// Spawn one interactive row under `parent`, plus its dropdown when open.
fn spawn_entry(
    commands: &mut Commands,
    parent: Entity,
    node: &MenuNode,
    by_parent: &HashMap<i64, Vec<&MenuNode>>,
    open: &[i64],
) {
    let caption = marker_caption(node);
    let entity = commands
        .spawn((
            Button,
            MenuEntry {
                handle: node.handle,
                depth: node.depth,
                has_children: node.has_children,
                enabled: node.enabled,
            },
            Node {
                height: Val::Px(MENU_ENTRY_HEIGHT),
                min_width: if node.depth == 0 {
                    Val::Auto
                } else {
                    Val::Px(MENU_ENTRY_MIN_WIDTH)
                },
                padding: UiRect::horizontal(Val::Px(8.0)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(entry_background(
                node.depth,
                node.enabled,
                Interaction::None,
            )),
        ))
        .id();
    commands.entity(parent).add_child(entity);

    let text_color = if node.enabled {
        Color::srgb(0.95, 0.95, 0.95)
    } else {
        Color::srgb(0.55, 0.55, 0.55)
    };
    let label = commands
        .spawn((
            Text::new(caption),
            TextFont::from_font_size(MENU_FONT_SIZE),
            TextColor(text_color),
            Node {
                flex_grow: 1.0,
                ..default()
            },
        ))
        .id();
    commands.entity(entity).add_child(label);

    if !node.shortcut.is_empty() {
        let shortcut = commands
            .spawn((
                Text::new(node.shortcut.clone()),
                TextFont::from_font_size(MENU_FONT_SIZE),
                TextColor(Color::srgb(0.65, 0.65, 0.68)),
            ))
            .id();
        commands.entity(entity).add_child(shortcut);
    }

    if node.has_children && open.contains(&node.handle) {
        let dropdown = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Percent(100.0),
                    left: Val::Px(0.0),
                    flex_direction: FlexDirection::Column,
                    min_width: Val::Px(MENU_ENTRY_MIN_WIDTH),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.13, 0.13, 0.16, 0.96)),
                ZIndex(1001),
            ))
            .id();
        commands.entity(entity).add_child(dropdown);
        if let Some(children) = by_parent.get(&node.handle) {
            for child in children {
                spawn_entry(commands, dropdown, child, by_parent, open);
            }
        }
    }
}

/// Reference-style leading marker for checked/radio items (a radio uses a dot
/// so it is distinguishable from a checkbox).
fn marker_caption(node: &MenuNode) -> String {
    if node.checked {
        if node.radio {
            format!("\u{25cf} {}", node.caption)
        } else {
            format!("\u{2713} {}", node.caption)
        }
    } else if node.radio {
        format!("\u{25cb} {}", node.caption)
    } else {
        node.caption.clone()
    }
}

fn entry_background(depth: u32, enabled: bool, interaction: Interaction) -> Color {
    let base = if depth == 0 {
        Color::NONE
    } else {
        Color::srgba(0.13, 0.13, 0.16, 0.96)
    };
    if !enabled {
        return base;
    }
    match interaction {
        Interaction::Hovered | Interaction::Pressed => Color::srgba(0.25, 0.42, 0.70, 0.95),
        Interaction::None => base,
    }
}

fn signature_of(nodes: &[MenuNode], open: &[i64]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    nodes.len().hash(&mut hasher);
    for node in nodes {
        node.hash(&mut hasher);
    }
    open.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(handle: i64, caption: &str, children: Vec<MenuItemSnapshot>) -> MenuItemSnapshot {
        MenuItemSnapshot {
            handle,
            caption: caption.to_string(),
            checked: false,
            enabled: true,
            radio: false,
            group: 0,
            visible: true,
            shortcut: String::new(),
            index: 0,
            children,
        }
    }

    fn sample_tree() -> MenuSnapshot {
        let mut open = item(11, "Open", vec![]);
        open.shortcut = "Ctrl+O".into();
        let recent = item(
            21,
            "Recent",
            vec![item(31, "File A", vec![]), item(32, "File B", vec![])],
        );
        let quit = item(12, "Quit", vec![]);
        MenuSnapshot {
            window: 0,
            root: MenuItemSnapshot {
                handle: 1,
                caption: String::new(),
                checked: false,
                enabled: true,
                radio: false,
                group: 0,
                visible: true,
                shortcut: String::new(),
                index: 0,
                children: vec![open, recent, quit],
            },
        }
    }

    #[test]
    fn visible_nodes_respects_open_path() {
        let view = sample_tree();
        let top = visible_nodes(&view, &[]);
        assert_eq!(
            top.iter().map(|n| n.caption.as_str()).collect::<Vec<_>>(),
            vec!["Open", "Recent", "Quit"]
        );
        assert!(top.iter().all(|n| n.depth == 0));
        assert_eq!(top[0].shortcut, "Ctrl+O");

        // Open the "Recent" submenu; its two children become visible.
        let nodes = visible_nodes(&view, &[21]);
        let captions: Vec<&str> = nodes.iter().map(|n| n.caption.as_str()).collect();
        assert_eq!(captions, vec!["Open", "Recent", "File A", "File B", "Quit"]);
        let file_a = nodes.iter().find(|n| n.caption == "File A").unwrap();
        assert_eq!(file_a.depth, 1);
        assert_eq!(file_a.parent, 21);
        assert!(!file_a.has_children);
    }

    #[test]
    fn path_to_handle_walks_ancestors() {
        let view = sample_tree();
        assert_eq!(path_to_handle(&view.root, 31), Some(vec![1, 21, 31]));
        assert_eq!(path_to_handle(&view.root, 12), Some(vec![1, 12]));
        assert_eq!(path_to_handle(&view.root, 999), None);
    }

    #[test]
    fn marker_caption_reflects_state() {
        let mut node = MenuNode {
            handle: 1,
            caption: "View".into(),
            shortcut: String::new(),
            checked: true,
            radio: false,
            enabled: true,
            has_children: false,
            depth: 0,
            parent: 0,
        };
        assert_eq!(marker_caption(&node), "\u{2713} View");
        node.checked = false;
        node.radio = true;
        assert_eq!(marker_caption(&node), "\u{25cb} View");
        node.checked = true;
        assert_eq!(marker_caption(&node), "\u{25cf} View");
    }
}
