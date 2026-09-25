//! Keeping the machine (and display) awake while a guard lives.
//! The Windows `SetThreadExecutionState` call is added by piece P14; until then
//! the guards are no-ops on every platform.

/// A keep-awake guard; released on drop.
#[derive(Debug)]
pub struct KeepAwake {
    _display: bool,
}

impl KeepAwake {
    /// Keeps the system from sleeping (queue running).
    pub fn system() -> KeepAwake {
        KeepAwake { _display: false }
    }

    /// Keeps the system and display on (player open).
    pub fn display() -> KeepAwake {
        KeepAwake { _display: true }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {}
}
