//! Keeping the machine (and display) awake while a guard lives.
//!
//! Guards are reference counted process-wide: the strongest live request wins
//! (display > system > none) and the state is only re-applied when that level
//! changes. `SetThreadExecutionState` is per thread, so on Windows the call is
//! made from one dedicated thread that lives for the whole process; guards can
//! then be created and dropped on any thread (tokio workers, the UI thread).
//! Elsewhere the level changes are only logged at debug.

use std::sync::{Mutex, PoisonError};

/// A keep-awake guard; released on drop.
#[derive(Debug)]
pub struct KeepAwake {
    display: bool,
}

impl KeepAwake {
    /// Keeps the system from sleeping (queue running).
    pub fn system() -> KeepAwake {
        change(false, true);
        KeepAwake { display: false }
    }

    /// Keeps the system and display on (player open).
    pub fn display() -> KeepAwake {
        change(true, true);
        KeepAwake { display: true }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        change(self.display, false);
    }
}

/// What the machine is currently asked to stay awake for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    /// Normal idle behaviour.
    Idle,
    /// `ES_SYSTEM_REQUIRED`: no idle sleep.
    System,
    /// `ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED`: no idle sleep, screen stays on.
    Display,
}

/// Live guard counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Holds {
    pub(crate) system: u32,
    pub(crate) display: u32,
}

impl Holds {
    pub(crate) fn level(self) -> Level {
        if self.display > 0 {
            Level::Display
        } else if self.system > 0 {
            Level::System
        } else {
            Level::Idle
        }
    }

    /// Applies one acquire or release and returns the new level when it changed.
    pub(crate) fn step(&mut self, display: bool, acquire: bool) -> Option<Level> {
        let before = self.level();
        let count = if display {
            &mut self.display
        } else {
            &mut self.system
        };
        *count = if acquire {
            count.saturating_add(1)
        } else {
            count.saturating_sub(1)
        };
        let after = self.level();
        (after != before).then_some(after)
    }
}

static HOLDS: Mutex<Holds> = Mutex::new(Holds {
    system: 0,
    display: 0,
});

/// Counts one guard in or out. The platform call is made while the lock is
/// held so level changes reach the OS in order.
fn change(display: bool, acquire: bool) {
    let mut holds = HOLDS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(level) = holds.step(display, acquire) {
        platform::apply(level);
    }
}

/// The current guard counts (tests and diagnostics).
#[cfg(test)]
pub(crate) fn holds() -> Holds {
    *HOLDS.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(windows)]
mod platform {
    use std::sync::mpsc::{self, Sender};
    use std::sync::OnceLock;

    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        EXECUTION_STATE,
    };

    use super::Level;

    static WORKER: OnceLock<Option<Sender<Level>>> = OnceLock::new();

    /// Hands the level to the power thread (started on first use). If that
    /// thread cannot be started, the state is set on the calling thread.
    pub(super) fn apply(level: Level) {
        let worker = WORKER.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Level>();
            let spawned = std::thread::Builder::new()
                .name("flox-power".into())
                .spawn(move || {
                    for level in rx {
                        set(level);
                    }
                });
            match spawned {
                Ok(_) => Some(tx),
                Err(err) => {
                    tracing::warn!(%err, "could not start the power thread");
                    None
                }
            }
        });
        match worker {
            Some(tx) if tx.send(level).is_ok() => {}
            _ => set(level),
        }
    }

    fn flags(level: Level) -> EXECUTION_STATE {
        match level {
            Level::Idle => ES_CONTINUOUS,
            Level::System => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
            Level::Display => ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
        }
    }

    fn set(level: Level) {
        // SAFETY: SetThreadExecutionState takes a flag set by value and touches
        // no memory owned by the caller.
        let previous = unsafe { SetThreadExecutionState(flags(level)) };
        if previous.0 == 0 {
            tracing::warn!(?level, "SetThreadExecutionState failed");
        } else {
            tracing::debug!(?level, "execution state set");
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Level;

    pub(super) fn apply(level: Level) {
        tracing::debug!(?level, "keep-awake level changed (no-op on this platform)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_strongest_hold() {
        let mut h = Holds::default();
        assert_eq!(h.step(false, true), Some(Level::System));
        assert_eq!(h.step(false, true), None);
        assert_eq!(h.step(true, true), Some(Level::Display));
        assert_eq!(h.step(false, false), None);
        assert_eq!(h.step(true, false), Some(Level::System));
        assert_eq!(h.step(false, false), Some(Level::Idle));
        assert_eq!(h, Holds::default());
    }

    #[test]
    fn release_without_hold_saturates() {
        let mut h = Holds::default();
        assert_eq!(h.step(true, false), None);
        assert_eq!(h, Holds::default());
    }

    /// The only test that creates real guards, so the global counts are stable.
    #[test]
    fn guards_count_in_and_out_across_threads() {
        let base = holds();
        let system = KeepAwake::system();
        let display = KeepAwake::display();
        assert_eq!(holds().system, base.system + 1);
        assert_eq!(holds().display, base.display + 1);
        assert_eq!(holds().level(), Level::Display);

        std::thread::spawn(move || drop(display))
            .join()
            .expect("join");
        assert_eq!(holds().display, base.display);
        assert_eq!(holds().level(), Level::System);

        drop(system);
        assert_eq!(holds(), base);
    }
}
