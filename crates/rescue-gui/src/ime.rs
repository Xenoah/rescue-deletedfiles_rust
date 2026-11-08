//! Normalize Windows cancellation at the egui-winit boundary.
//!
//! winit reports an empty Preedit with no cursor before cancelling (and before
//! committing). egui-winit 0.33 maps that to Disabled, while TextEdit leaves the
//! selected preedit text intact on Disabled. Remove only an outstanding preedit
//! in its original focused widget; a subsequent Commit still inserts normally.
use eframe::egui::{Event, Id, ImeEvent, Key};

#[derive(Default)]
pub struct Composition {
    owner: Option<Id>,
    escape_held: bool,
}

impl Composition {
    pub fn normalize(&mut self, focused: Option<Id>, events: &mut Vec<Event>) {
        // egui's focus pass handles Escape before TextEdit sees IME events.
        // Keep the composing field focused until its preedit is removed, even
        // when Disabled and Escape arrive in either order in this input batch.
        let ime_owns_escape = focused.is_some()
            && (self.owner == focused
                || events.iter().any(|event| {
                    matches!(
                        event,
                        Event::Ime(ImeEvent::Enabled | ImeEvent::Preedit(_) | ImeEvent::Commit(_))
                    )
                }));
        let mut normalized = Vec::with_capacity(events.len());
        for event in events.drain(..) {
            if let Event::Key {
                key: Key::Escape,
                pressed,
                ..
            } = &event
            {
                if ime_owns_escape || self.escape_held {
                    self.escape_held = *pressed;
                    continue;
                }
            }
            if matches!(event, Event::WindowFocused(false)) {
                self.escape_held = false;
            }
            match &event {
                Event::Ime(ImeEvent::Enabled | ImeEvent::Commit(_)) => self.owner = None,
                Event::Ime(ImeEvent::Preedit(text)) => {
                    self.owner = (!text.is_empty()).then_some(focused).flatten();
                }
                Event::Ime(ImeEvent::Disabled)
                    if self
                        .owner
                        .take()
                        .is_some_and(|owner| Some(owner) == focused) =>
                {
                    normalized.push(Event::Ime(ImeEvent::Preedit(String::new())));
                }
                _ => {}
            }
            normalized.push(event);
        }
        *events = normalized;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_cannot_delete_a_different_widgets_selection() {
        let mut composition = Composition::default();
        let mut events = vec![Event::Ime(ImeEvent::Preedit("未確定".into()))];
        composition.normalize(Some(Id::new("first")), &mut events);
        let mut events = vec![Event::Ime(ImeEvent::Disabled)];
        composition.normalize(Some(Id::new("second")), &mut events);
        assert_eq!(events, vec![Event::Ime(ImeEvent::Disabled)]);
        composition.normalize(Some(Id::new("first")), &mut events);
        assert_eq!(events, vec![Event::Ime(ImeEvent::Disabled)]);
    }

    fn escape(pressed: bool, repeat: bool) -> Event {
        Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed,
            repeat,
            modifiers: eframe::egui::Modifiers::NONE,
        }
    }

    #[test]
    fn ime_escape_repeat_and_release_do_not_become_a_new_focus_escape() {
        let focused = Some(Id::new("edit"));
        let mut composition = Composition::default();
        let mut events = vec![Event::Ime(ImeEvent::Preedit("未確定".into()))];
        composition.normalize(focused, &mut events);
        let mut events = vec![escape(true, false)];
        composition.normalize(focused, &mut events);
        assert!(events.is_empty());
        let mut events = vec![Event::Ime(ImeEvent::Disabled)];
        composition.normalize(focused, &mut events);
        assert_eq!(
            events,
            vec![
                Event::Ime(ImeEvent::Preedit(String::new())),
                Event::Ime(ImeEvent::Disabled)
            ]
        );
        for event in [escape(true, true), escape(false, false)] {
            let mut events = vec![event];
            composition.normalize(focused, &mut events);
            assert!(events.is_empty());
        }
        let expected = vec![Event::Ime(ImeEvent::Disabled), escape(true, false)];
        let mut events = expected.clone();
        composition.normalize(focused, &mut events);
        assert_eq!(events, expected);
    }

    #[test]
    fn stale_composition_does_not_capture_another_widgets_escape() {
        let mut composition = Composition::default();
        let mut events = vec![Event::Ime(ImeEvent::Preedit("未確定".into()))];
        composition.normalize(Some(Id::new("first")), &mut events);
        let expected = vec![escape(true, false), Event::Ime(ImeEvent::Disabled)];
        let mut events = expected.clone();
        composition.normalize(Some(Id::new("second")), &mut events);
        assert_eq!(events, expected);
    }

}
