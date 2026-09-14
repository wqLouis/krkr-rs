//! In-engine menu bar / dropdowns / context popups drawn with Bevy UI from the
//! native `MenuItem` tree.
//!
//! The Linux/Bevy host has no native OS menu, so this module renders the
//! logical tree exposed by `crates/tvp-natives/src/menu_item.rs` as a real
//! in-app menu:
//!
//! * a horizontal bar of top-level items for every registered window menu
//!   ([`menu_snapshots`]), and
//! * a floating context popup for `MenuItem.popup(flags, x, y)`
//!   ([`take_popup_request`]), positioned at the requested client coordinates.
//!
//! Dropdown columns for `popup`/hover-opened submenus, checked/radio/disabled
//! state and shortcut text are rendered on each row. Activating a leaf calls
//! [`tvp_natives::fire_menu_click`], which runs the script `onClick` handler
//! exactly like the reference's `fireClick`
//! (`reference/cpp/core/visual/MenuItemImpl.cpp:305`).
//!
//! Re-rendering is driven by a signature: whenever the logical tree, the open
//! path or the popup request changes, the whole menu entity tree is rebuilt.
//!
//! # Visibility
//!
//! The bar only appears when a registered root has at least one *visible*
//! top-level item. The real game defines native `MenuItem`s only for its
//! debug hotkeys (`k2compat.tjs` `createDebugShortcutMenuItem`) and sets
//! `visible = false` on every one of them, so those do not surface as UI (the
//! reference hides invisible items the same way). A tree with visible items
//! renders immediately; see the tests and [`visible_nodes`].

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
/// Stack order for the top-level bar (above any game UI).
const MENU_BAR_Z: i32 = 1000;
/// Stack order for a context popup (above the bar / click-away backdrop).
const MENU_POPUP_Z: i32 = 1001;
/// Stack order for the click-away backdrop under a context popup.
const MENU_BACKDROP_Z: i32 = 999;

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
    /// `0` for a top-level bar item, `1+` for dropdown / popup rows.
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

/// Marker for the spawned menu bar / popup container (and its backdrop).
#[derive(Component)]
pub struct MenuRoot;

/// Marker for the full-screen click-away backdrop shown under a context popup.
#[derive(Component)]
pub struct MenuBackdrop;

/// Component attached to every interactive menu row.
#[derive(Component, Clone, Copy, Debug)]
pub struct MenuEntry {
    pub handle: i64,
    pub depth: u32,
    pub has_children: bool,
    pub enabled: bool,
}

/// An active context popup: the item's captured tree plus the requested
/// client-space anchor.
#[derive(Debug)]
struct PopupState {
    x: f32,
    y: f32,
    item: MenuItemSnapshot,
}

/// Renderer-side state: the currently open dropdown path, the active context
/// popup, the entity of the last built menu, and the signature used to avoid
/// needless rebuilds.
#[derive(Resource, Default)]
pub struct MenuUiState {
    signature: u64,
    open: Vec<i64>,
    root: Option<Entity>,
    popup: Option<PopupState>,
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
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut entries: Query<(&Interaction, &MenuEntry, &mut BackgroundColor), Changed<Interaction>>,
    backdrop: Query<&Interaction, (With<MenuBackdrop>, Changed<Interaction>)>,
) {
    // Escape closes any open bar dropdown or context popup.
    if keys.is_some_and(|keys| keys.just_pressed(KeyCode::Escape)) {
        state.open.clear();
        state.popup = None;
    }

    // A press anywhere on the click-away backdrop dismisses the popup.
    for interaction in &backdrop {
        if *interaction == Interaction::Pressed {
            state.popup = None;
            state.open.clear();
        }
    }

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
                    state.popup = None;
                    if !fire_menu_click(handle) {
                        log::debug!("menu: handle {handle} did not fire (disabled or gone)");
                    }
                }
            }
            Interaction::None => {}
        }
    }

    if let Some((handle, x, y, item)) = take_popup_request() {
        // A context popup owns the screen while it is open: drop any bar
        // dropdown path and remember the requested item + position. The
        // popup item itself is the root of the open path so nested submenus
        // truncate correctly at depth 1.
        state.open = vec![handle];
        state.popup = Some(PopupState {
            x: x as f32,
            y: y as f32,
            item,
        });
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

/// Rebuild the Bevy UI when the tree, open path or popup changes.
fn menu_sync_ui(
    mut commands: Commands,
    mut state: ResMut<MenuUiState>,
    existing: Query<Entity, With<MenuRoot>>,
) {
    // Resolve a context popup first; it takes over the whole menu surface.
    // Otherwise flatten every registered window root into the top-level bar.
    let mut nodes: Vec<MenuNode> = Vec::new();
    let mut popup_anchor: Option<Vec2> = None;
    let mut popup_parent = 0i64;
    let popup = state.popup.take();
    let mut keep_popup = None;
    if let Some(popup) = popup {
        if !popup.item.children.is_empty() {
            for child in &popup.item.children {
                push_node(child, 1, popup.item.handle, &state.open, &mut nodes);
            }
            if nodes.is_empty() {
                // The item exists but all of its rows are hidden.
                state.open.clear();
            } else {
                popup_anchor = Some(Vec2::new(popup.x, popup.y));
                popup_parent = popup.item.handle;
                keep_popup = Some(popup);
            }
        } else {
            // A leaf item has no popup to show.
            state.open.clear();
        }
    }
    state.popup = keep_popup;
    if popup_anchor.is_none() {
        for view in &menu_snapshots() {
            nodes.extend(visible_nodes(view, &state.open));
        }
    }

    let signature = signature_of(&nodes, &state.open, popup_anchor);

    // The root may have been removed by a previous frame; treat a missing
    // entity as "needs rebuild".
    let root_alive = state.root.is_some_and(|e| existing.contains(e));
    if root_alive && signature == state.signature {
        return;
    }
    // A rebuild replaces the whole surface; `despawn` also removes the popup
    // backdrop and every row.
    for entity in &existing {
        commands.entity(entity).despawn();
    }
    state.root = None;
    state.signature = signature;
    if nodes.is_empty() {
        return;
    }

    // Group dropdown children by their parent handle.
    let mut by_parent: HashMap<i64, Vec<&MenuNode>> = HashMap::new();
    for node in &nodes {
        by_parent.entry(node.parent).or_default().push(node);
    }
    let root_parent = if popup_anchor.is_some() {
        popup_parent
    } else {
        0
    };
    let Some(top) = by_parent.get(&root_parent) else {
        return;
    };

    if let Some(anchor) = popup_anchor {
        // Full-screen click-away backdrop behind the popup.
        commands.spawn((
            MenuRoot,
            MenuBackdrop,
            Button,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(Color::NONE),
            GlobalZIndex(MENU_BACKDROP_Z),
        ));
        let root = commands
            .spawn((
                MenuRoot,
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(anchor.y),
                    left: Val::Px(anchor.x),
                    flex_direction: FlexDirection::Column,
                    min_width: Val::Px(MENU_ENTRY_MIN_WIDTH),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.13, 0.13, 0.16, 0.98)),
                GlobalZIndex(MENU_POPUP_Z),
            ))
            .id();
        for node in top {
            spawn_entry(&mut commands, root, node, &by_parent, &state.open);
        }
        state.root = Some(root);
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
            GlobalZIndex(MENU_BAR_Z),
        ))
        .id();
    for node in top {
        spawn_entry(&mut commands, root, node, &by_parent, &state.open);
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
        // A top-level bar item drops its menu below itself; every deeper
        // submenu (and every popup row) opens to the right, like the
        // reference's cascading menus.
        let (top, left) = if node.depth == 0 {
            (Val::Percent(100.0), Val::Px(0.0))
        } else {
            (Val::Px(0.0), Val::Percent(100.0))
        };
        let dropdown = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top,
                    left,
                    flex_direction: FlexDirection::Column,
                    min_width: Val::Px(MENU_ENTRY_MIN_WIDTH),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.13, 0.13, 0.16, 0.96)),
                GlobalZIndex(MENU_POPUP_Z),
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

fn signature_of(nodes: &[MenuNode], open: &[i64], popup: Option<Vec2>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    nodes.len().hash(&mut hasher);
    for node in nodes {
        node.hash(&mut hasher);
    }
    open.hash(&mut hasher);
    popup
        .map(|anchor| (anchor.x.to_bits(), anchor.y.to_bits()))
        .hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::*;
    use tjs2_sys::{Tjs2Engine, TjsValue};

    /// The VM and the menu registry are process-global; serialize the tests
    /// that build a real `MenuItem` tree.
    static MENU_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_lock() -> MutexGuard<'static, ()> {
        MENU_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn engine() -> &'static Tjs2Engine {
        let engine = Box::leak(Box::new(Tjs2Engine::new().unwrap()));
        tvp_natives::register_all(engine).unwrap();
        engine
    }

    fn menu_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(MenuPlugin);
        app
    }

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

    fn entries(world: &mut World) -> Vec<(Entity, MenuEntry)> {
        let mut query = world.query::<(Entity, &MenuEntry)>();
        query.iter(world).map(|(e, m)| (e, *m)).collect()
    }

    fn non_backdrop_roots(world: &mut World) -> Vec<Node> {
        let mut query = world.query_filtered::<&Node, (With<MenuRoot>, Without<MenuBackdrop>)>();
        query.iter(world).cloned().collect()
    }

    /// The floating context popup (a vertical column), if one is on screen.
    fn popup_root(world: &mut World) -> Option<Node> {
        non_backdrop_roots(world)
            .into_iter()
            .find(|node| node.flex_direction == FlexDirection::Column)
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

    /// A registered window tree with visible items produces a top-level bar
    /// with one row per visible item; hidden and disabled state is preserved.
    #[test]
    fn top_level_bar_renders_visible_tree() {
        let _lock = test_lock();
        let engine = engine();
        engine
            .exec_script(
                "var w = %[id: 501];\
                 var root = __krkr_make_window_menu(w);\
                 var alpha = new MenuItem(null, 'Alpha');\
                 var hidden = new MenuItem(null, 'Hidden'); hidden.visible = false;\
                 var gamma = new MenuItem(null, 'Gamma'); gamma.enabled = false;\
                 var delta = new MenuItem(null, 'Delta'); delta.checked = true; delta.shortcut = 'Ctrl+D';\
                 root.add(alpha); root.add(hidden); root.add(gamma); root.add(delta);",
                "menu-bar",
            )
            .unwrap();

        let mut app = menu_app();
        app.update();

        let world = app.world_mut();
        let rows = entries(world);
        assert_eq!(rows.len(), 3, "the hidden top-level item must not render");
        assert!(rows.iter().all(|(_, m)| m.depth == 0));

        let node = non_backdrop_roots(world)
            .into_iter()
            .next()
            .expect("the bar root must exist");
        assert_eq!(node.top, Val::Px(0.0));
        assert_eq!(node.left, Val::Px(0.0));
        assert_eq!(node.width, Val::Percent(100.0));
        assert_eq!(node.height, Val::Px(MENU_BAR_HEIGHT));

        // The disabled "Gamma" row is present but marked disabled.
        assert_eq!(rows.iter().filter(|(_, m)| !m.enabled).count(), 1);
        // "Delta" has a shortcut and a checked marker in its caption.
        let delta = tvp_natives::menu_snapshots()
            .into_iter()
            .find(|s| s.window == 501)
            .and_then(|s| s.root.children.into_iter().find(|c| c.caption == "Delta"))
            .expect("Delta must be registered");
        let delta_node = visible_nodes(&tvp_natives::menu_snapshot(501).unwrap(), &[])
            .into_iter()
            .find(|n| n.handle == delta.handle)
            .unwrap();
        assert!(delta_node.checked);
        assert_eq!(delta_node.shortcut, "Ctrl+D");
        assert_eq!(marker_caption(&delta_node), "\u{2713} Delta");
    }

    /// `popup(flags, x, y)` renders the item's submenu at the requested client
    /// position (not the top-left corner), and pressing a leaf fires its
    /// `onClick` and closes the popup.
    #[test]
    fn popup_renders_at_requested_position_and_activates() {
        let _lock = test_lock();
        let engine = engine();
        engine
            .exec_script(
                "var fired = 0;\
                 var root = new MenuItem(null, 'Context');\
                 var copy = new MenuItem(null, 'Copy');\
                 copy.onClick = function() { fired++; };\
                 root.add(copy);\
                 var paste = new MenuItem(null, 'Paste'); paste.enabled = false;\
                 root.add(paste);\
                 root.popup(0, 40, 60);",
                "menu-popup",
            )
            .unwrap();

        let mut app = menu_app();
        app.update();

        {
            let world = app.world_mut();
            let node = popup_root(world).expect("the popup root must exist");
            assert_eq!(
                node.left,
                Val::Px(40.0),
                "popup must honour the requested x"
            );
            assert_eq!(node.top, Val::Px(60.0), "popup must honour the requested y");
            assert_eq!(
                node.flex_direction,
                FlexDirection::Column,
                "a popup is a vertical column"
            );
            let rows = entries(world);
            assert_eq!(rows.len(), 2, "the popup shows the item's two children");
            assert!(
                rows.iter().all(|(_, m)| m.depth == 1),
                "popup rows are dropdown rows, not bar rows"
            );
            assert_eq!(rows.iter().filter(|(_, m)| !m.enabled).count(), 1);
        }

        // Press the enabled "Copy" leaf: the script callback fires and the
        // popup is dismissed.
        let copy = {
            let world = app.world_mut();
            entries(world)
                .into_iter()
                .find(|(_, m)| m.enabled)
                .map(|(e, _)| e)
                .expect("the enabled Copy leaf must be present")
        };
        app.world_mut()
            .entity_mut(copy)
            .insert(Interaction::Pressed);
        app.update();
        assert_eq!(
            engine.eval("fired", "menu-popup").unwrap(),
            TjsValue::Integer(1),
            "pressing the popup row must run the script onClick"
        );
        app.update();
        assert!(
            popup_root(app.world_mut()).is_none(),
            "the popup must close after activating a leaf"
        );
    }

    /// A popup requested for a leaf item (no children) is ignored rather than
    /// rendering an empty floating box.
    #[test]
    fn popup_on_leaf_renders_nothing() {
        let _lock = test_lock();
        let engine = engine();
        engine
            .exec_script(
                "var leaf = new MenuItem(null, 'Leaf'); leaf.popup(0, 5, 5);",
                "menu-leaf-popup",
            )
            .unwrap();
        let mut app = menu_app();
        app.update();
        assert!(popup_root(app.world_mut()).is_none());
    }

    /// `open_at` keeps only the path prefix, so hovering a sibling replaces
    /// the previous dropdown instead of appending to it.
    #[test]
    fn open_at_replaces_siblings() {
        let mut open = vec![1i64, 2];
        open_at(&mut open, 3, 0);
        assert_eq!(open, vec![3]);
        open_at(&mut open, 4, 1);
        assert_eq!(open, vec![3, 4]);
        open_at(&mut open, 5, 1);
        assert_eq!(open, vec![3, 5]);
    }
}
