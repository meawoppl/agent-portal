use std::cell::RefCell;
use std::rc::Rc;

use uuid::Uuid;
use yew::prelude::*;

const BROADCAST_WINDOW_MS: f64 = 2_400.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AgentMessageBroadcast {
    pub from_session_id: Uuid,
    pub to_session_id: Uuid,
    pub timestamp: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RailAxis {
    Horizontal,
    Vertical,
}

impl RailAxis {
    fn class(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct BroadcastView {
    pub from_session_id: Uuid,
    pub to_session_id: Uuid,
    pub timestamp: f64,
    pub start_pct: f64,
    pub end_pct: f64,
    pub reverse: bool,
}

#[derive(Clone)]
pub struct BroadcastRef(Rc<RefCell<Vec<AgentMessageBroadcast>>>);

impl BroadcastRef {
    pub fn push(&self, event: AgentMessageBroadcast) {
        let cutoff = event.timestamp - BROADCAST_WINDOW_MS;
        let mut events = self.0.borrow_mut();
        events.retain(|e| e.timestamp > cutoff);
        events.push(event);
    }

    pub(super) fn active_session_ids(&self, now: f64) -> (Vec<Uuid>, Vec<Uuid>) {
        let cutoff = now - BROADCAST_WINDOW_MS;
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for event in self.0.borrow().iter().filter(|e| e.timestamp > cutoff) {
            push_unique(&mut senders, event.from_session_id);
            push_unique(&mut receivers, event.to_session_id);
        }
        (senders, receivers)
    }

    pub(super) fn view_for(&self, rendered_session_ids: &[Uuid], now: f64) -> Vec<BroadcastView> {
        if rendered_session_ids.len() < 2 {
            return Vec::new();
        }

        let cutoff = now - BROADCAST_WINDOW_MS;
        self.0
            .borrow()
            .iter()
            .filter(|event| event.timestamp > cutoff)
            .filter_map(|event| {
                let from_idx = rendered_session_ids
                    .iter()
                    .position(|id| *id == event.from_session_id)?;
                let to_idx = rendered_session_ids
                    .iter()
                    .position(|id| *id == event.to_session_id)?;
                if from_idx == to_idx {
                    return None;
                }
                let from_pct = slot_pct(from_idx, rendered_session_ids.len());
                let to_pct = slot_pct(to_idx, rendered_session_ids.len());
                Some(BroadcastView {
                    from_session_id: event.from_session_id,
                    to_session_id: event.to_session_id,
                    timestamp: event.timestamp,
                    start_pct: from_pct.min(to_pct),
                    end_pct: from_pct.max(to_pct),
                    reverse: from_pct > to_pct,
                })
            })
            .collect()
    }
}

impl PartialEq for BroadcastRef {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Default for BroadcastRef {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(Vec::new())))
    }
}

pub(super) fn render_broadcasts(
    broadcasts: &BroadcastRef,
    rendered_session_ids: &[Uuid],
    axis: RailAxis,
    render_time: f64,
) -> Html {
    let views = broadcasts.view_for(rendered_session_ids, render_time);
    if views.is_empty() {
        return html! {};
    }

    html! {
        <div class={classes!("agent-broadcast-layer", axis.class())} aria-hidden="true">
            { for views.into_iter().map(|view| {
                let span = (view.end_pct - view.start_pct).max(1.0);
                let style = match axis {
                    RailAxis::Horizontal => {
                        format!("left: {:.2}%; width: {:.2}%;", view.start_pct, span)
                    }
                    RailAxis::Vertical => {
                        format!("top: {:.2}%; height: {:.2}%;", view.start_pct, span)
                    }
                };
                html! {
                    <span
                        key={format!("{}-{}-{:.0}", view.from_session_id, view.to_session_id, view.timestamp)}
                        class={classes!("agent-broadcast-path", view.reverse.then_some("reverse"))}
                        {style}
                    >
                        <span class="agent-broadcast-packet" />
                    </span>
                }
            }) }
        </div>
    }
}

fn slot_pct(index: usize, total: usize) -> f64 {
    ((index as f64) + 0.5) / (total as f64) * 100.0
}

fn push_unique(values: &mut Vec<Uuid>, id: Uuid) {
    if !values.contains(&id) {
        values.push(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn broadcast_view_maps_visible_sender_and_receiver_to_slots() {
        let events = BroadcastRef::default();
        events.push(AgentMessageBroadcast {
            from_session_id: id(1),
            to_session_id: id(3),
            timestamp: 1_000.0,
        });

        let view = events.view_for(&[id(1), id(2), id(3)], 1_200.0);
        assert_eq!(view.len(), 1);
        assert!((view[0].start_pct - 16.666).abs() < 0.01);
        assert!((view[0].end_pct - 83.333).abs() < 0.01);
        assert!(!view[0].reverse);
    }

    #[test]
    fn broadcast_view_marks_reverse_direction() {
        let events = BroadcastRef::default();
        events.push(AgentMessageBroadcast {
            from_session_id: id(3),
            to_session_id: id(1),
            timestamp: 1_000.0,
        });

        let view = events.view_for(&[id(1), id(2), id(3)], 1_200.0);
        assert_eq!(view.len(), 1);
        assert!(view[0].reverse);
    }

    #[test]
    fn broadcast_view_omits_events_when_sender_is_not_rendered() {
        let events = BroadcastRef::default();
        events.push(AgentMessageBroadcast {
            from_session_id: id(9),
            to_session_id: id(1),
            timestamp: 1_000.0,
        });

        assert!(events.view_for(&[id(1), id(2)], 1_200.0).is_empty());
    }

    #[test]
    fn active_session_ids_expires_old_events() {
        let events = BroadcastRef::default();
        events.push(AgentMessageBroadcast {
            from_session_id: id(1),
            to_session_id: id(2),
            timestamp: 1_000.0,
        });

        let (senders, receivers) = events.active_session_ids(1_100.0);
        assert_eq!(senders, vec![id(1)]);
        assert_eq!(receivers, vec![id(2)]);

        let (senders, receivers) = events.active_session_ids(4_000.0);
        assert!(senders.is_empty());
        assert!(receivers.is_empty());
    }
}
