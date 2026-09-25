//! Spatial focus: zones (top bar, rows, grids, lists, modals) with per-zone memory
//! of the last focused child. Filled in by piece P15.

/// An arrow-key direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// The focus graph for one screen.
#[derive(Clone, Debug, Default)]
pub struct FocusGraph {
    _private: (),
}
