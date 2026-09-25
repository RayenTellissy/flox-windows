//! Spatial focus: zones (top bar, rows, grids, lists, modals) with per-zone memory
//! of the last focused child.
//!
//! Slint's own focus is tab-order only, so the logical focus lives here and the UI
//! only draws it. The pattern, as used by `ui/gallery.slint` and the screens:
//!
//! - The root element of a window is a `FocusRoot` (`ui/components/focus.slint`), a
//!   `FocusScope` that holds Slint's real keyboard focus and forwards every key press
//!   to Rust through `capture-key-pressed`. Rust maps it with [`key_action`], moves the
//!   [`FocusGraph`] and writes the result to the `FocusState` global (`zone`, `index`,
//!   `editing`). Returning `true` accepts the key, `false` lets it continue to the
//!   focused element.
//! - Every focusable component takes `zone` and `index` properties and draws its
//!   2 px ring when they equal `FocusState.zone` / `FocusState.index`.
//! - Text fields are the only elements that take real Slint focus. When the logical
//!   focus lands on a `TextField`, the field calls `focus()` on its `TextInput`
//!   (a `changed focused` handler). While it has focus, key presses still pass the
//!   root's `capture-key-pressed` first, because Slint runs the capture phase from the
//!   window down to the focused item. [`key_action`] with `editing = true` claims only
//!   Up, Down, Esc and Ctrl shortcuts, so the field keeps Left/Right, Backspace,
//!   Enter, Space and typed text, and focus can still leave it with Up/Down/Esc.
//!   When Rust clears `FocusState.editing`, `FocusRoot` takes the real focus back.
//! - Mouse: `FocusArea` reports pointer moves (not enter events, so a list scrolling
//!   under a still pointer does not steal focus), clicks and right clicks through
//!   `FocusState.hovered` / `clicked` / `menu`. Rust answers with
//!   [`FocusGraph::hover`] and [`FocusGraph::click`].
//! - Dialogs call [`FocusGraph::push_modal`]; focus stays inside until
//!   [`FocusGraph::pop_modal`] restores the focus the screen had.
//! - Scrolling follows focus with [`ensure_visible`] / [`Strip`], computed from the
//!   known item sizes and written to a Flickable's `viewport-x` / `viewport-y`
//!   (which are the negated scroll offsets).

use slint::platform::Key;

/// An arrow-key direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Identifies a zone. Mirrors the `zone` int property of the Slint components.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ZoneId(pub i32);

/// How a zone lays out its items, which decides how arrows move inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneKind {
    /// The screen's top bar. Moves like a row.
    TopBar,
    /// A horizontal row: Left/Right move inside and stop at the ends, Up/Down leave.
    Row,
    /// A vertical list: Up/Down move inside and leave at the ends. Left/Right are
    /// not consumed, so screens can use them (for example to cycle a setting).
    List,
    /// A grid filled row by row with `columns` items per row (see [`grid_columns`]).
    Grid { columns: usize },
}

/// A focused item: a zone and an index inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Focus {
    pub zone: ZoneId,
    pub index: usize,
}

impl Focus {
    pub const fn new(zone: ZoneId, index: usize) -> Self {
        Self { zone, index }
    }

    /// The `(zone, index)` pair as Slint ints.
    pub fn to_slint(self) -> (i32, i32) {
        (self.zone.0, i32::try_from(self.index).unwrap_or(i32::MAX))
    }
}

/// One focusable group of items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    id: ZoneId,
    kind: ZoneKind,
    len: usize,
    last_index: usize,
    text: bool,
}

impl Zone {
    pub fn new(id: ZoneId, kind: ZoneKind, len: usize) -> Self {
        Self {
            id,
            kind,
            len,
            last_index: 0,
            text: false,
        }
    }

    pub fn top_bar(id: ZoneId, len: usize) -> Self {
        Self::new(id, ZoneKind::TopBar, len)
    }

    pub fn row(id: ZoneId, len: usize) -> Self {
        Self::new(id, ZoneKind::Row, len)
    }

    pub fn list(id: ZoneId, len: usize) -> Self {
        Self::new(id, ZoneKind::List, len)
    }

    pub fn grid(id: ZoneId, columns: usize, len: usize) -> Self {
        Self::new(id, ZoneKind::Grid { columns }, len)
    }

    /// Marks the zone's items as text fields: focusing one gives it real keyboard focus.
    pub fn text(mut self) -> Self {
        self.text = true;
        self
    }

    /// Starts the zone's memory at `index` (Android `FocusRow.preferPosition`).
    pub fn prefer(mut self, index: usize) -> Self {
        self.last_index = index;
        self
    }

    pub fn id(&self) -> ZoneId {
        self.id
    }

    pub fn kind(&self) -> ZoneKind {
        self.kind
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn last_index(&self) -> usize {
        self.last_index
    }

    /// The index focus lands on when the zone is entered: the memory, clamped.
    fn entry_index(&self) -> usize {
        self.last_index.min(self.len.saturating_sub(1))
    }

    fn columns(&self) -> usize {
        match self.kind {
            ZoneKind::Grid { columns } => columns.max(1),
            _ => 1,
        }
    }
}

/// A set of zones in declared (top to bottom) order, with its own focus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Layer {
    zones: Vec<Zone>,
    focus: Option<Focus>,
}

impl Layer {
    fn position(&self, id: ZoneId) -> Option<usize> {
        self.zones.iter().position(|z| z.id == id)
    }

    fn zone(&self, id: ZoneId) -> Option<&Zone> {
        self.zones.iter().find(|z| z.id == id)
    }

    fn zone_mut(&mut self, id: ZoneId) -> Option<&mut Zone> {
        self.zones.iter_mut().find(|z| z.id == id)
    }

    /// Focuses `index` in zone `id` and records it as the zone's memory.
    fn land(&mut self, id: ZoneId, index: usize) -> Option<Focus> {
        let zone = self.zone_mut(id)?;
        if index >= zone.len {
            return None;
        }
        zone.last_index = index;
        let focus = Focus::new(id, index);
        self.focus = Some(focus);
        Some(focus)
    }

    /// Enters zone `id` at its remembered index.
    fn enter(&mut self, id: ZoneId) -> Option<Focus> {
        let zone = self.zone(id)?;
        if zone.is_empty() {
            return None;
        }
        let index = zone.entry_index();
        self.land(id, index)
    }

    fn first_non_empty(&self) -> Option<ZoneId> {
        self.zones.iter().find(|z| !z.is_empty()).map(|z| z.id)
    }

    /// The nearest non-empty zone above (`up`) or below the zone at `from`.
    fn neighbour(&self, from: usize, up: bool) -> Option<ZoneId> {
        let found = if up {
            self.zones[..from].iter().rev().find(|z| !z.is_empty())
        } else {
            self.zones.get(from + 1..)?.iter().find(|z| !z.is_empty())
        };
        found.map(|z| z.id)
    }

    /// Moves focus one step. Returns the new focus when it changed.
    fn step(&mut self, direction: Direction) -> Option<Focus> {
        let Some(current) = self.focus else {
            let id = self.first_non_empty()?;
            return self.enter(id);
        };
        let pos = self.position(current.zone)?;
        let zone = &self.zones[pos];
        let index = current.index;
        let len = zone.len;

        let inside = match (zone.kind, direction) {
            (ZoneKind::TopBar | ZoneKind::Row, Direction::Left) => Some(index.checked_sub(1)),
            (ZoneKind::TopBar | ZoneKind::Row, Direction::Right) => {
                Some((index + 1 < len).then_some(index + 1))
            }
            (ZoneKind::List, Direction::Up) => index.checked_sub(1).map(Some),
            (ZoneKind::List, Direction::Down) => (index + 1 < len).then_some(Some(index + 1)),
            (ZoneKind::List, Direction::Left | Direction::Right) => Some(None),
            (ZoneKind::Grid { .. }, _) => grid_step(zone.columns(), len, index, direction),
            (_, Direction::Up | Direction::Down) => None,
        };
        match inside {
            // Stays in the zone: a new index, or blocked at an edge.
            Some(Some(next)) => self.land(current.zone, next),
            Some(None) => None,
            // Leaves the zone for the neighbour above or below.
            None => {
                let up = direction == Direction::Up;
                let target = self.neighbour(pos, up)?;
                self.enter(target)
            }
        }
    }

    /// Re-validates the focus after zone lengths changed.
    fn repair(&mut self) {
        let Some(current) = self.focus else {
            return;
        };
        let Some(pos) = self.position(current.zone) else {
            self.focus = self.first_non_empty().and_then(|id| self.enter(id));
            return;
        };
        let zone = &self.zones[pos];
        if current.index < zone.len {
            return;
        }
        if !zone.is_empty() {
            let last = zone.len - 1;
            self.land(current.zone, last);
            return;
        }
        let target = self
            .neighbour(pos, false)
            .or_else(|| self.neighbour(pos, true));
        self.focus = None;
        if let Some(id) = target {
            self.enter(id);
        }
    }
}

/// Grid movement inside the zone. `Some(Some(i))` moves, `Some(None)` is blocked,
/// `None` leaves the zone vertically.
fn grid_step(
    columns: usize,
    len: usize,
    index: usize,
    direction: Direction,
) -> Option<Option<usize>> {
    let column = index % columns;
    let row = index / columns;
    let last_row = len.saturating_sub(1) / columns;
    match direction {
        Direction::Left => Some((column > 0).then(|| index - 1)),
        Direction::Right => Some((column + 1 < columns && index + 1 < len).then_some(index + 1)),
        Direction::Up => (row > 0).then(|| Some(index - columns)),
        Direction::Down => {
            if index + columns < len {
                Some(Some(index + columns))
            } else if row < last_row {
                // The row below is partial and has nothing under this column.
                Some(Some(len - 1))
            } else {
                None
            }
        }
    }
}

/// The focus graph for one screen: zones in declared order plus a stack of modal
/// layers. Only the top layer can hold focus, which is how dialogs trap it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusGraph {
    layers: Vec<Layer>,
    saved: Vec<Option<Focus>>,
}

impl Default for FocusGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusGraph {
    pub fn new() -> Self {
        Self {
            layers: vec![Layer::default()],
            saved: Vec::new(),
        }
    }

    /// A graph over `zones`, top to bottom, with nothing focused yet.
    pub fn with_zones(zones: Vec<Zone>) -> Self {
        Self {
            layers: vec![Layer { zones, focus: None }],
            saved: Vec::new(),
        }
    }

    fn top(&self) -> &Layer {
        // `layers` is never empty: `new` creates the base and `pop_modal` keeps it.
        &self.layers[self.layers.len() - 1]
    }

    fn top_mut(&mut self) -> &mut Layer {
        let last = self.layers.len() - 1;
        &mut self.layers[last]
    }

    /// Appends a zone below the others in the current layer.
    pub fn add_zone(&mut self, zone: Zone) {
        let layer = self.top_mut();
        layer.zones.retain(|z| z.id != zone.id);
        layer.zones.push(zone);
    }

    /// Removes a zone from whichever layer holds it; focus moves on if it was there.
    pub fn remove_zone(&mut self, id: ZoneId) {
        for layer in &mut self.layers {
            if let Some(pos) = layer.position(id) {
                let neighbour = layer
                    .neighbour(pos, false)
                    .or_else(|| layer.neighbour(pos, true));
                layer.zones.remove(pos);
                if layer.focus.is_some_and(|f| f.zone == id) {
                    layer.focus = None;
                    if let Some(next) = neighbour {
                        layer.enter(next);
                    }
                }
            }
        }
    }

    /// The zone `id` in any layer.
    pub fn zone(&self, id: ZoneId) -> Option<&Zone> {
        self.layers.iter().rev().find_map(|l| l.zone(id))
    }

    /// Changes a zone's item count (a row finished loading, a list shrank). A focus
    /// past the end is clamped; a focus in a zone that became empty moves to the
    /// nearest non-empty zone below, else above.
    pub fn set_len(&mut self, id: ZoneId, len: usize) {
        for layer in &mut self.layers {
            if let Some(zone) = layer.zone_mut(id) {
                zone.len = len;
                layer.repair();
            }
        }
    }

    /// Changes a grid's column count (the window was resized).
    pub fn set_columns(&mut self, id: ZoneId, columns: usize) {
        for layer in &mut self.layers {
            if let Some(zone) = layer.zone_mut(id) {
                if let ZoneKind::Grid { .. } = zone.kind {
                    zone.kind = ZoneKind::Grid { columns };
                }
            }
        }
    }

    /// Sets a zone's memory without focusing it (Details preselects the season).
    pub fn remember(&mut self, id: ZoneId, index: usize) {
        for layer in &mut self.layers {
            if let Some(zone) = layer.zone_mut(id) {
                zone.last_index = index;
            }
        }
    }

    /// The zone's memory, if the zone exists.
    pub fn last_index(&self, id: ZoneId) -> Option<usize> {
        self.zone(id).map(Zone::last_index)
    }

    /// The focused item of the top layer.
    pub fn focus(&self) -> Option<Focus> {
        self.top().focus
    }

    /// True when the focused item is a text field (see [`Zone::text`]).
    pub fn editing(&self) -> bool {
        self.focus()
            .and_then(|f| self.top().zone(f.zone))
            .is_some_and(|z| z.text)
    }

    /// Focuses the first non-empty zone at its remembered index.
    pub fn focus_first(&mut self) -> Option<Focus> {
        let layer = self.top_mut();
        let id = layer.first_non_empty()?;
        layer.enter(id)
    }

    /// Enters zone `id` at its remembered index (Search: BACK returns to the input).
    pub fn focus_zone(&mut self, id: ZoneId) -> Option<Focus> {
        self.top_mut().enter(id)
    }

    /// Focuses an exact item. Refused (returns false) for zones outside the top
    /// layer, which keeps a dialog's focus trapped, and for indices out of range.
    pub fn set_focus(&mut self, id: ZoneId, index: usize) -> bool {
        self.top_mut().land(id, index).is_some()
    }

    /// The pointer moved over an item: focus follows it. Returns the new focus
    /// when it changed.
    pub fn hover(&mut self, id: ZoneId, index: usize) -> Option<Focus> {
        let target = Focus::new(id, index);
        if self.focus() == Some(target) {
            return None;
        }
        self.set_focus(id, index).then_some(target)
    }

    /// The item was clicked: focus it and return it so the caller activates it.
    /// Clicks outside an open dialog return `None`.
    pub fn click(&mut self, id: ZoneId, index: usize) -> Option<Focus> {
        self.set_focus(id, index).then_some(Focus::new(id, index))
    }

    /// Moves focus one step. Returns the new focus when it changed; `None` means the
    /// key moved nothing (an edge, or Left/Right in a list) and the screen may use it.
    pub fn move_focus(&mut self, direction: Direction) -> Option<Focus> {
        self.top_mut().step(direction)
    }

    /// Opens a modal layer over the current zones and focuses its first item.
    pub fn push_modal(&mut self, zones: Vec<Zone>) -> Option<Focus> {
        self.saved.push(self.focus());
        self.layers.push(Layer { zones, focus: None });
        self.focus_first()
    }

    /// Closes the top modal layer and restores the focus from before it opened.
    /// Returns false when no modal is open.
    pub fn pop_modal(&mut self) -> bool {
        if self.layers.len() < 2 {
            return false;
        }
        self.layers.pop();
        let restored = self.saved.pop().flatten();
        let layer = self.top_mut();
        layer.focus = restored;
        layer.repair();
        if layer.focus.is_none() {
            if let Some(id) = layer.first_non_empty() {
                layer.enter(id);
            }
        }
        true
    }

    pub fn in_modal(&self) -> bool {
        self.layers.len() > 1
    }
}

/// Items per row of a poster grid: `floor(width / (item + gap))`, at least 1.
/// Android's Search uses `(screen width − 96) / 176`, which is this with
/// `width = screen − 96`, `item = 160`, `gap = 16`.
pub fn grid_columns(width: f32, item: f32, gap: f32) -> usize {
    let pitch = item + gap;
    if !(width > 0.0 && pitch > 0.0) {
        return 1;
    }
    // Truncation is intended: whole items only.
    ((width / pitch).floor() as usize).max(1)
}

/// The scroll offset (≥ 0) that shows `[item_start, item_start + item_len)` inside a
/// viewport of `viewport` px over `content` px, moving as little as possible. Write
/// it to a Flickable as `viewport-x: -offset` (or `viewport-y`).
pub fn ensure_visible(
    scroll: f32,
    viewport: f32,
    content: f32,
    item_start: f32,
    item_len: f32,
) -> f32 {
    let max = (content - viewport).max(0.0);
    let item_end = item_start + item_len;
    let wanted = if item_start < scroll || item_len >= viewport {
        item_start
    } else if item_end > scroll + viewport {
        item_end - viewport
    } else {
        scroll
    };
    wanted.clamp(0.0, max)
}

/// Equal items laid out in a line with a fixed gap and leading padding, the shape of
/// a poster row or a list of fixed-height rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strip {
    pub lead: f32,
    pub item: f32,
    pub gap: f32,
}

impl Strip {
    /// A row of 160 px posters, 16 px apart.
    pub const POSTERS: Self = Self {
        lead: 0.0,
        item: 160.0,
        gap: 16.0,
    };

    pub fn start(&self, index: usize) -> f32 {
        self.lead + index as f32 * (self.item + self.gap)
    }

    /// The content length for `count` items, without a trailing gap.
    pub fn content(&self, count: usize) -> f32 {
        if count == 0 {
            return self.lead;
        }
        self.start(count - 1) + self.item + self.lead
    }

    /// The scroll offset that keeps item `index` of `count` visible.
    pub fn ensure_visible(&self, scroll: f32, viewport: f32, count: usize, index: usize) -> f32 {
        ensure_visible(
            scroll,
            viewport,
            self.content(count),
            self.start(index),
            self.item,
        )
    }
}

/// Keyboard modifiers of a key press, as Slint reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

/// What a key press means to the app (the global keys table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Move(Direction),
    /// Enter.
    Center,
    /// Esc, or Backspace outside text fields.
    Back,
    /// M or the context-menu key.
    Menu,
    /// Play/pause in the player, toggle selection in lists.
    Space,
    /// Ctrl+F.
    Search,
    /// Ctrl+,.
    Settings,
    /// F11.
    Fullscreen,
    /// Ctrl+R (long-press MENU on Android).
    Reload,
}

/// Maps a key press (`KeyEvent.text` plus modifiers) to an action. While `editing`
/// (a text field has real focus), only Up, Down, Esc and the Ctrl/F11 shortcuts are
/// claimed; everything else returns `None` so the root lets it through to the field.
pub fn key_action(text: &str, modifiers: Modifiers, editing: bool) -> Option<KeyAction> {
    let mut chars = text.chars();
    let key = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let is = |k: Key| key == char::from(k);

    if modifiers.control && !modifiers.alt {
        return match key.to_ascii_lowercase() {
            'f' => Some(KeyAction::Search),
            ',' => Some(KeyAction::Settings),
            'r' => Some(KeyAction::Reload),
            _ => None,
        };
    }
    if is(Key::F11) {
        return Some(KeyAction::Fullscreen);
    }
    if is(Key::UpArrow) {
        return Some(KeyAction::Move(Direction::Up));
    }
    if is(Key::DownArrow) {
        return Some(KeyAction::Move(Direction::Down));
    }
    if is(Key::Escape) {
        return Some(KeyAction::Back);
    }
    if editing || modifiers.alt || modifiers.meta {
        return None;
    }
    if is(Key::LeftArrow) {
        Some(KeyAction::Move(Direction::Left))
    } else if is(Key::RightArrow) {
        Some(KeyAction::Move(Direction::Right))
    } else if is(Key::Return) {
        Some(KeyAction::Center)
    } else if is(Key::Backspace) {
        Some(KeyAction::Back)
    } else if is(Key::Space) {
        Some(KeyAction::Space)
    } else if is(Key::Menu) || key.eq_ignore_ascii_case(&'m') {
        Some(KeyAction::Menu)
    } else {
        None
    }
}

/// Filters pointer moves so focus follows the mouse only after it really moved
/// (Slint can report a move with an unchanged position after the content scrolled).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerGate {
    last: Option<(f32, f32)>,
}

impl PointerGate {
    /// Records the position; true when it differs from the previous one.
    pub fn moved(&mut self, x: f32, y: f32) -> bool {
        let moved = self.last != Some((x, y));
        self.last = Some((x, y));
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: ZoneId = ZoneId(0);
    const CONTINUE: ZoneId = ZoneId(1);
    const LIBRARY: ZoneId = ZoneId(2);
    const MOVIES: ZoneId = ZoneId(3);
    const GRID: ZoneId = ZoneId(4);
    const LIST: ZoneId = ZoneId(5);

    fn home() -> FocusGraph {
        FocusGraph::with_zones(vec![
            Zone::top_bar(BAR, 4),
            Zone::row(CONTINUE, 3),
            Zone::row(LIBRARY, 0),
            Zone::row(MOVIES, 20),
        ])
    }

    fn at(zone: ZoneId, index: usize) -> Option<Focus> {
        Some(Focus::new(zone, index))
    }

    #[test]
    fn focus_first_arrow_focuses_first_item() {
        let mut g = home();
        assert_eq!(g.focus(), None);
        assert_eq!(g.move_focus(Direction::Down), at(BAR, 0));
    }

    #[test]
    fn focus_first_skips_empty_zones() {
        let mut g = FocusGraph::with_zones(vec![Zone::row(LIBRARY, 0), Zone::row(MOVIES, 2)]);
        assert_eq!(g.focus_first(), at(MOVIES, 0));
    }

    #[test]
    fn focus_row_left_right_and_edges() {
        let mut g = home();
        g.set_focus(CONTINUE, 0);
        assert_eq!(g.move_focus(Direction::Left), None);
        assert_eq!(g.move_focus(Direction::Right), at(CONTINUE, 1));
        assert_eq!(g.move_focus(Direction::Right), at(CONTINUE, 2));
        assert_eq!(g.move_focus(Direction::Right), None);
        assert_eq!(g.focus(), at(CONTINUE, 2));
    }

    #[test]
    fn focus_down_between_zones_in_declared_order() {
        let mut g = home();
        g.set_focus(BAR, 2);
        assert_eq!(g.move_focus(Direction::Down), at(CONTINUE, 0));
        assert_eq!(g.move_focus(Direction::Up), at(BAR, 2));
    }

    #[test]
    fn focus_skips_empty_zone() {
        let mut g = home();
        g.set_focus(CONTINUE, 1);
        assert_eq!(g.move_focus(Direction::Down), at(MOVIES, 0));
        assert_eq!(g.move_focus(Direction::Up), at(CONTINUE, 1));
    }

    #[test]
    fn focus_row_memory_restored_on_entry() {
        let mut g = home();
        g.set_focus(MOVIES, 7);
        g.move_focus(Direction::Up);
        assert_eq!(g.focus(), at(CONTINUE, 0));
        assert_eq!(g.move_focus(Direction::Down), at(MOVIES, 7));
        assert_eq!(g.last_index(MOVIES), Some(7));
    }

    #[test]
    fn focus_row_memory_clamped_to_new_length() {
        let mut g = home();
        g.set_focus(MOVIES, 15);
        g.move_focus(Direction::Up);
        g.set_len(MOVIES, 4);
        assert_eq!(g.move_focus(Direction::Down), at(MOVIES, 3));
    }

    #[test]
    fn focus_clamped_when_focused_zone_shrinks() {
        let mut g = home();
        g.set_focus(MOVIES, 15);
        g.set_len(MOVIES, 10);
        assert_eq!(g.focus(), at(MOVIES, 9));
    }

    #[test]
    fn focus_moves_on_when_focused_zone_empties() {
        let mut g = home();
        g.set_focus(CONTINUE, 2);
        g.set_len(CONTINUE, 0);
        assert_eq!(g.focus(), at(MOVIES, 0));
        g.set_len(MOVIES, 0);
        assert_eq!(g.focus(), at(BAR, 0));
    }

    #[test]
    fn focus_zone_filled_later_becomes_reachable() {
        let mut g = home();
        g.set_focus(CONTINUE, 0);
        g.set_len(LIBRARY, 5);
        assert_eq!(g.move_focus(Direction::Down), at(LIBRARY, 0));
    }

    #[test]
    fn focus_prefer_sets_entry_index() {
        let mut g =
            FocusGraph::with_zones(vec![Zone::row(BAR, 1), Zone::row(CONTINUE, 6).prefer(3)]);
        g.set_focus(BAR, 0);
        assert_eq!(g.move_focus(Direction::Down), at(CONTINUE, 3));
        g.remember(CONTINUE, 5);
        g.move_focus(Direction::Up);
        assert_eq!(g.move_focus(Direction::Down), at(CONTINUE, 5));
    }

    #[test]
    fn focus_edges_of_screen_block() {
        let mut g = home();
        g.set_focus(BAR, 0);
        assert_eq!(g.move_focus(Direction::Up), None);
        g.set_focus(MOVIES, 3);
        assert_eq!(g.move_focus(Direction::Down), None);
        assert_eq!(g.focus(), at(MOVIES, 3));
    }

    fn grid(len: usize) -> FocusGraph {
        FocusGraph::with_zones(vec![Zone::row(BAR, 2), Zone::grid(GRID, 6, len)])
    }

    #[test]
    fn focus_grid_left_right_stay_in_row() {
        let mut g = grid(14);
        g.set_focus(GRID, 6);
        assert_eq!(g.move_focus(Direction::Left), None);
        g.set_focus(GRID, 11);
        assert_eq!(g.move_focus(Direction::Right), None);
        assert_eq!(g.move_focus(Direction::Left), at(GRID, 10));
        g.set_focus(GRID, 13);
        assert_eq!(g.move_focus(Direction::Right), None);
    }

    #[test]
    fn focus_grid_up_down_by_columns() {
        let mut g = grid(14);
        g.set_focus(GRID, 2);
        assert_eq!(g.move_focus(Direction::Down), at(GRID, 8));
        assert_eq!(g.move_focus(Direction::Up), at(GRID, 2));
    }

    #[test]
    fn focus_grid_up_from_first_row_leaves() {
        let mut g = grid(14);
        g.set_focus(GRID, 4);
        assert_eq!(g.move_focus(Direction::Up), at(BAR, 0));
        assert_eq!(g.move_focus(Direction::Down), at(GRID, 4));
    }

    #[test]
    fn focus_grid_down_into_partial_row_takes_last() {
        let mut g = grid(14);
        g.set_focus(GRID, 10);
        assert_eq!(g.move_focus(Direction::Down), at(GRID, 13));
    }

    #[test]
    fn focus_grid_down_from_last_row_blocked_or_leaves() {
        let mut g = grid(14);
        g.set_focus(GRID, 13);
        assert_eq!(g.move_focus(Direction::Down), None);
        g.add_zone(Zone::row(LIST, 1));
        assert_eq!(g.move_focus(Direction::Down), at(LIST, 0));
    }

    #[test]
    fn focus_grid_columns_change_on_resize() {
        let mut g = grid(14);
        g.set_columns(GRID, 4);
        g.set_focus(GRID, 1);
        assert_eq!(g.move_focus(Direction::Down), at(GRID, 5));
    }

    #[test]
    fn focus_list_up_down_and_exit() {
        let mut g = FocusGraph::with_zones(vec![Zone::row(BAR, 1), Zone::list(LIST, 3)]);
        g.set_focus(LIST, 0);
        assert_eq!(g.move_focus(Direction::Down), at(LIST, 1));
        assert_eq!(g.move_focus(Direction::Left), None);
        assert_eq!(g.move_focus(Direction::Right), None);
        assert_eq!(g.move_focus(Direction::Up), at(LIST, 0));
        assert_eq!(g.move_focus(Direction::Up), at(BAR, 0));
        assert_eq!(g.move_focus(Direction::Down), at(LIST, 0));
    }

    #[test]
    fn focus_modal_traps_and_restores() {
        let mut g = home();
        g.set_focus(MOVIES, 4);
        let buttons = ZoneId(100);
        let field = ZoneId(101);
        assert_eq!(
            g.push_modal(vec![Zone::list(field, 1).text(), Zone::row(buttons, 2)]),
            at(field, 0)
        );
        assert!(g.in_modal());
        assert!(g.editing());
        assert_eq!(g.move_focus(Direction::Up), None);
        assert_eq!(g.move_focus(Direction::Down), at(buttons, 0));
        assert_eq!(g.move_focus(Direction::Right), at(buttons, 1));
        assert_eq!(g.move_focus(Direction::Right), None);
        assert_eq!(g.move_focus(Direction::Down), None);
        assert!(!g.set_focus(MOVIES, 1));
        assert_eq!(g.click(BAR, 0), None);
        assert_eq!(g.hover(MOVIES, 2), None);
        assert!(g.pop_modal());
        assert!(!g.in_modal());
        assert_eq!(g.focus(), at(MOVIES, 4));
        assert!(!g.pop_modal());
    }

    #[test]
    fn focus_modal_restore_clamps_if_zone_shrank() {
        let mut g = home();
        g.set_focus(MOVIES, 9);
        g.push_modal(vec![Zone::row(ZoneId(100), 2)]);
        g.set_len(MOVIES, 3);
        g.pop_modal();
        assert_eq!(g.focus(), at(MOVIES, 2));
    }

    #[test]
    fn focus_hover_and_click_set_focus() {
        let mut g = home();
        assert_eq!(g.hover(MOVIES, 5), at(MOVIES, 5));
        assert_eq!(g.hover(MOVIES, 5), None);
        assert_eq!(g.hover(MOVIES, 99), None);
        assert_eq!(g.click(CONTINUE, 2), at(CONTINUE, 2));
        assert_eq!(g.focus(), at(CONTINUE, 2));
        assert_eq!(g.last_index(CONTINUE), Some(2));
    }

    #[test]
    fn focus_remove_zone_moves_focus() {
        let mut g = home();
        g.set_focus(CONTINUE, 1);
        g.remove_zone(CONTINUE);
        assert_eq!(g.focus(), at(MOVIES, 0));
        assert!(g.zone(CONTINUE).is_none());
    }

    #[test]
    fn focus_editing_follows_text_zones() {
        let search = ZoneId(9);
        let mut g =
            FocusGraph::with_zones(vec![Zone::row(search, 1).text(), Zone::grid(GRID, 6, 3)]);
        g.focus_first();
        assert!(g.editing());
        g.move_focus(Direction::Down);
        assert!(!g.editing());
        assert_eq!(g.focus_zone(search), at(search, 0));
        assert!(g.editing());
    }

    #[test]
    fn focus_grid_columns_from_width() {
        assert_eq!(grid_columns(1920.0 - 96.0, 160.0, 16.0), 10);
        assert_eq!(grid_columns(1280.0 - 96.0, 160.0, 16.0), 6);
        assert_eq!(grid_columns(100.0, 160.0, 16.0), 1);
        assert_eq!(grid_columns(0.0, 160.0, 16.0), 1);
    }

    #[test]
    fn focus_ensure_visible_offsets() {
        // Already visible: unchanged.
        assert_eq!(ensure_visible(0.0, 1000.0, 3000.0, 176.0, 160.0), 0.0);
        // Past the right edge: scroll just enough.
        assert_eq!(ensure_visible(0.0, 1000.0, 3000.0, 1056.0, 160.0), 216.0);
        // Before the left edge: align the start.
        assert_eq!(ensure_visible(500.0, 1000.0, 3000.0, 352.0, 160.0), 352.0);
        // Clamped to the content end and to zero.
        assert_eq!(ensure_visible(0.0, 1000.0, 1100.0, 1000.0, 160.0), 100.0);
        assert_eq!(ensure_visible(0.0, 1000.0, 500.0, 300.0, 160.0), 0.0);
    }

    #[test]
    fn focus_strip_posters() {
        let s = Strip::POSTERS;
        assert_eq!(s.start(3), 528.0);
        assert_eq!(s.content(20), 20.0 * 176.0 - 16.0);
        assert_eq!(s.ensure_visible(0.0, 1184.0, 20, 5), 0.0);
        assert_eq!(
            s.ensure_visible(0.0, 1184.0, 20, 6),
            6.0 * 176.0 + 160.0 - 1184.0
        );
        assert_eq!(s.ensure_visible(800.0, 1184.0, 20, 2), 352.0);
    }

    #[test]
    fn focus_key_actions_browse() {
        let none = Modifiers::default();
        let k = |key: Key| String::from(char::from(key));
        assert_eq!(
            key_action(&k(Key::LeftArrow), none, false),
            Some(KeyAction::Move(Direction::Left))
        );
        assert_eq!(
            key_action(&k(Key::Return), none, false),
            Some(KeyAction::Center)
        );
        assert_eq!(
            key_action(&k(Key::Backspace), none, false),
            Some(KeyAction::Back)
        );
        assert_eq!(
            key_action(&k(Key::Escape), none, false),
            Some(KeyAction::Back)
        );
        assert_eq!(key_action(" ", none, false), Some(KeyAction::Space));
        assert_eq!(key_action("m", none, false), Some(KeyAction::Menu));
        assert_eq!(
            key_action(&k(Key::Menu), none, false),
            Some(KeyAction::Menu)
        );
        assert_eq!(
            key_action(&k(Key::F11), none, false),
            Some(KeyAction::Fullscreen)
        );
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(key_action("f", ctrl, false), Some(KeyAction::Search));
        assert_eq!(key_action(",", ctrl, false), Some(KeyAction::Settings));
        assert_eq!(key_action("r", ctrl, false), Some(KeyAction::Reload));
        assert_eq!(key_action("x", none, false), None);
        assert_eq!(key_action("", none, false), None);
    }

    #[test]
    fn focus_key_actions_while_editing() {
        let none = Modifiers::default();
        let k = |key: Key| String::from(char::from(key));
        assert_eq!(
            key_action(&k(Key::UpArrow), none, true),
            Some(KeyAction::Move(Direction::Up))
        );
        assert_eq!(
            key_action(&k(Key::DownArrow), none, true),
            Some(KeyAction::Move(Direction::Down))
        );
        assert_eq!(
            key_action(&k(Key::Escape), none, true),
            Some(KeyAction::Back)
        );
        assert_eq!(key_action(&k(Key::LeftArrow), none, true), None);
        assert_eq!(key_action(&k(Key::Backspace), none, true), None);
        assert_eq!(key_action(&k(Key::Return), none, true), None);
        assert_eq!(key_action(" ", none, true), None);
        assert_eq!(key_action("m", none, true), None);
    }

    #[test]
    fn focus_pointer_gate_ignores_still_pointer() {
        let mut gate = PointerGate::default();
        assert!(gate.moved(10.0, 10.0));
        assert!(!gate.moved(10.0, 10.0));
        assert!(gate.moved(11.0, 10.0));
    }
}
