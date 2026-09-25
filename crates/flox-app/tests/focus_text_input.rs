//! Checks the root FocusScope pattern against a real TextInput (plan risk R5):
//! `capture-key-pressed` on `FocusRoot` sees every key before the focused
//! TextInput, Rust claims Up/Down/Esc while editing, and everything else still
//! reaches the field (typing, Left, Backspace).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use flox_app::focus::{key_action, Direction, KeyAction, Modifiers};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, PhysicalSize, SharedString};

slint::slint! {
    import { FocusRoot, FocusState } from "../ui/components/focus.slint";
    import { TextField } from "../ui/components/text_field.slint";

    export { FocusState }

    export component KeyProbe inherits Window {
        width: 400px;
        height: 120px;

        callback key(text: string, control: bool, shift: bool, alt: bool, meta: bool, repeat: bool) -> bool;
        out property <string> field-text: field.text;
        out property <bool> field-has-focus: field.has-input-focus;

        FocusRoot {
            key(text, control, shift, alt, meta, repeat) => {
                root.key(text, control, shift, alt, meta, repeat)
            }

            field := TextField {
                x: 20px;
                y: 20px;
                width: 360px;
                zone: 1;
                index: 0;
            }
        }
    }
}

struct TestPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn press(probe: &KeyProbe, text: impl Into<SharedString>) {
    let text = text.into();
    let window = probe.window();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
    slint::platform::update_timers_and_animations();
}

fn set_focus(probe: &KeyProbe, zone: i32, editing: bool) {
    let state = probe.global::<FocusState>();
    state.set_zone(zone);
    state.set_index(if zone >= 0 { 0 } else { -1 });
    state.set_editing(editing);
    slint::platform::update_timers_and_animations();
}

#[test]
fn focus_root_captures_keys_before_text_input() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform {
        window: window.clone(),
    }))
    .unwrap();
    let probe = KeyProbe::new().unwrap();
    window.set_size(PhysicalSize::new(400, 120));
    probe.show().unwrap();

    // Rust's side of the root: map the key, record claimed actions, and leave the
    // field on Up/Down/Esc the way a screen would.
    let editing = Rc::new(Cell::new(false));
    let claimed = Rc::new(RefCell::new(Vec::<KeyAction>::new()));
    let seen = Rc::new(Cell::new(0usize));
    {
        let editing = editing.clone();
        let claimed = claimed.clone();
        let seen = seen.clone();
        let weak = probe.as_weak();
        probe.on_key(move |text, control, shift, alt, meta, _repeat| {
            seen.set(seen.get() + 1);
            let modifiers = Modifiers {
                control,
                shift,
                alt,
                meta,
            };
            let Some(action) = key_action(&text, modifiers, editing.get()) else {
                return false;
            };
            claimed.borrow_mut().push(action);
            if editing.get() {
                if let Some(probe) = weak.upgrade() {
                    editing.set(false);
                    let state = probe.global::<FocusState>();
                    state.set_zone(-1);
                    state.set_index(-1);
                    state.set_editing(false);
                }
            }
            true
        });
    }

    // Nothing logical is focused: the root holds real focus and typing goes nowhere.
    press(&probe, "a");
    assert_eq!(probe.get_field_text(), "");
    assert!(!probe.get_field_has_focus());
    assert_eq!(seen.get(), 1, "the root saw the key");

    // Logical focus lands on the field: it takes real focus.
    editing.set(true);
    set_focus(&probe, 1, true);
    assert!(probe.get_field_has_focus(), "the field took real focus");

    // Typing passes the root (not claimed) and reaches the TextInput.
    press(&probe, "a");
    press(&probe, "b");
    assert_eq!(probe.get_field_text(), "ab");

    // Left is not claimed while editing: it moves the caret.
    press(&probe, Key::LeftArrow);
    press(&probe, "X");
    assert_eq!(probe.get_field_text(), "aXb");

    // Backspace is not BACK while editing: it deletes.
    press(&probe, Key::Backspace);
    assert_eq!(probe.get_field_text(), "ab");
    assert!(claimed.borrow().is_empty(), "nothing claimed while typing");
    assert_eq!(seen.get(), 6, "the root saw every key first");

    // Up is claimed at the root before the TextInput sees it, and focus leaves.
    press(&probe, Key::UpArrow);
    assert_eq!(
        claimed.borrow().as_slice(),
        &[KeyAction::Move(Direction::Up)]
    );
    assert_eq!(probe.get_field_text(), "ab");
    assert!(!probe.get_field_has_focus(), "the root took focus back");

    // Back in the field, Down and Esc are claimed the same way.
    editing.set(true);
    set_focus(&probe, 1, true);
    assert!(probe.get_field_has_focus());
    press(&probe, Key::DownArrow);
    assert!(!probe.get_field_has_focus());

    editing.set(true);
    set_focus(&probe, 1, true);
    press(&probe, Key::Escape);
    assert!(!probe.get_field_has_focus());
    assert_eq!(
        claimed.borrow().as_slice(),
        &[
            KeyAction::Move(Direction::Up),
            KeyAction::Move(Direction::Down),
            KeyAction::Back,
        ]
    );
    assert_eq!(probe.get_field_text(), "ab");

    // With the root focused again, arrows are plain navigation keys.
    press(&probe, Key::RightArrow);
    assert_eq!(
        claimed.borrow().last(),
        Some(&KeyAction::Move(Direction::Right))
    );
}
