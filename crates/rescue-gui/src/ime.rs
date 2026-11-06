//! Normalize Windows cancellation at the egui-winit boundary.
//!
//! winit reports an empty Preedit with no cursor before cancelling (and before
//! committing). egui-winit 0.33 maps that to Disabled, while TextEdit leaves the
//! selected preedit text intact on Disabled. Remove only an outstanding preedit
//! in its original focused widget; a subsequent Commit still inserts normally.
use eframe::egui::{Event, Id, ImeEvent};

#[derive(Default)]
pub struct Composition {
    owner: Option<Id>,
}

impl Composition {
    pub fn normalize(&mut self, focused: Option<Id>, events: &mut Vec<Event>) {
        let mut normalized = Vec::with_capacity(events.len());
        for event in events.drain(..) {
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
}
