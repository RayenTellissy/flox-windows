//! The component gallery, driven by the spatial focus engine.
//!
//! `cargo run -p flox-app --example gallery`
//!
//! Arrows move between and inside zones (declared top to bottom in column order),
//! the poster row scrolls to keep focus visible, Enter reports the activated item,
//! Esc leaves a text field, and the mouse focuses on move and activates on click.

use std::cell::RefCell;
use std::rc::Rc;

use flox_app::focus::{
    key_action, Direction, Focus, FocusGraph, KeyAction, Modifiers, Strip, Zone, ZoneId,
};
use slint::ComponentHandle;

slint::slint! {
    export { Gallery, FocusState } from "../ui/gallery.slint";
}

// Mirrors `GalleryZones` in ui/gallery.slint.
const TOP_BAR: ZoneId = ZoneId(0);
const POSTERS: ZoneId = ZoneId(1);
const EPISODES: ZoneId = ZoneId(2);
const FILLED: ZoneId = ZoneId(3);
const GHOST: ZoneId = ZoneId(4);
const SEASONS: ZoneId = ZoneId(5);
const SEARCH: ZoneId = ZoneId(6);
const SETTINGS: ZoneId = ZoneId(7);
const DIALOG: ZoneId = ZoneId(8);
const SEEK: ZoneId = ZoneId(9);
const PLAYER: ZoneId = ZoneId(10);
const KEY_FIELD: ZoneId = ZoneId(11);
const POSTER_COUNT: usize = 5;

fn graph() -> FocusGraph {
    FocusGraph::with_zones(vec![
        Zone::top_bar(TOP_BAR, 4),
        Zone::row(POSTERS, POSTER_COUNT),
        Zone::list(EPISODES, 2),
        Zone::row(FILLED, 2),
        Zone::row(GHOST, 2),
        Zone::row(SEASONS, 2),
        Zone::row(SEARCH, 1).text(),
        Zone::list(SETTINGS, 2),
        Zone::row(DIALOG, 2),
        Zone::row(SEEK, 1),
        Zone::row(PLAYER, 3),
        Zone::row(KEY_FIELD, 1).text(),
    ])
}

/// Writes the graph's focus to the UI and scrolls the poster row to it.
fn sync(gallery: &Gallery, graph: &FocusGraph) {
    let state = gallery.global::<FocusState>();
    let (zone, index) = graph.focus().map(Focus::to_slint).unwrap_or((-1, -1));
    state.set_zone(zone);
    state.set_index(index);
    state.set_editing(graph.editing());

    if let Some(focus) = graph.focus().filter(|f| f.zone == POSTERS) {
        let scroll = -gallery.get_posters_viewport_x();
        let viewport = gallery.get_posters_viewport_width();
        let offset = Strip::POSTERS.ensure_visible(scroll, viewport, POSTER_COUNT, focus.index);
        gallery.set_posters_viewport_x(-offset);
    }
}

fn activate(focus: Focus) {
    println!("activate zone {} item {}", focus.zone.0, focus.index);
}

fn main() -> Result<(), slint::PlatformError> {
    let gallery = Gallery::new()?;
    gallery.set_forced(false);
    let graph = Rc::new(RefCell::new(graph()));
    graph.borrow_mut().focus_first();
    sync(&gallery, &graph.borrow());

    let weak = gallery.as_weak();
    let keys = graph.clone();
    gallery.on_key(move |text, control, shift, alt, meta, _repeat| {
        let Some(gallery) = weak.upgrade() else {
            return false;
        };
        let mut graph = keys.borrow_mut();
        let modifiers = Modifiers {
            control,
            shift,
            alt,
            meta,
        };
        let Some(action) = key_action(&text, modifiers, graph.editing()) else {
            return false;
        };
        match action {
            KeyAction::Move(direction) => {
                graph.move_focus(direction);
            }
            KeyAction::Center => {
                if let Some(focus) = graph.focus() {
                    activate(focus);
                }
            }
            KeyAction::Back => {
                // Esc in a text field leaves it for the next zone down.
                if graph.editing() {
                    graph.move_focus(Direction::Down);
                } else {
                    println!("back");
                }
            }
            other => println!("{other:?}"),
        }
        sync(&gallery, &graph);
        true
    });

    let state = gallery.global::<FocusState>();
    let weak = gallery.as_weak();
    let hovers = graph.clone();
    state.on_hovered(move |zone, index| {
        let (Some(gallery), Ok(index)) = (weak.upgrade(), usize::try_from(index)) else {
            return;
        };
        let mut graph = hovers.borrow_mut();
        if graph.hover(ZoneId(zone), index).is_some() {
            sync(&gallery, &graph);
        }
    });

    let weak = gallery.as_weak();
    let clicks = graph.clone();
    state.on_clicked(move |zone, index| {
        let (Some(gallery), Ok(index)) = (weak.upgrade(), usize::try_from(index)) else {
            return;
        };
        let mut graph = clicks.borrow_mut();
        if let Some(focus) = graph.click(ZoneId(zone), index) {
            sync(&gallery, &graph);
            activate(focus);
        }
    });

    state.on_menu(|zone, index| println!("menu zone {zone} item {index}"));

    gallery.run()
}
