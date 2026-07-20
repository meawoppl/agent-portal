use std::cell::RefCell;
use std::rc::Rc;

use shared::AgentType;
use uuid::Uuid;
use web_sys::Element;
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
    pub start: f64,
    pub end: f64,
    pub units: BroadcastUnits,
    pub reverse: bool,
    pub agent_type: AgentType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BroadcastUnits {
    Percent,
    Pixels,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RenderedSessionPosition {
    pub id: Uuid,
    pub agent_type: AgentType,
    pub center: f64,
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

    #[cfg(test)]
    pub(super) fn view_for(&self, rendered_session_ids: &[Uuid], now: f64) -> Vec<BroadcastView> {
        let rendered_sessions: Vec<_> = rendered_session_ids
            .iter()
            .map(|id| (*id, AgentType::Claude))
            .collect();
        self.view_for_sessions(&rendered_sessions, now)
    }

    pub(super) fn view_for_sessions(
        &self,
        rendered_sessions: &[(Uuid, AgentType)],
        now: f64,
    ) -> Vec<BroadcastView> {
        let rendered_session_ids: Vec<Uuid> = rendered_sessions.iter().map(|(id, _)| *id).collect();
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
                    start: from_pct.min(to_pct),
                    end: from_pct.max(to_pct),
                    units: BroadcastUnits::Percent,
                    reverse: from_pct > to_pct,
                    agent_type: rendered_sessions
                        .get(from_idx)
                        .map(|(_, agent_type)| *agent_type)
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    pub(super) fn view_for_positions(
        &self,
        rendered_positions: &[RenderedSessionPosition],
        now: f64,
    ) -> Vec<BroadcastView> {
        if rendered_positions.len() < 2 {
            return Vec::new();
        }

        let cutoff = now - BROADCAST_WINDOW_MS;
        self.0
            .borrow()
            .iter()
            .filter(|event| event.timestamp > cutoff)
            .filter_map(|event| {
                let from = rendered_positions
                    .iter()
                    .find(|pos| pos.id == event.from_session_id)?;
                let to = rendered_positions
                    .iter()
                    .find(|pos| pos.id == event.to_session_id)?;
                if from.id == to.id {
                    return None;
                }
                Some(BroadcastView {
                    from_session_id: event.from_session_id,
                    to_session_id: event.to_session_id,
                    timestamp: event.timestamp,
                    start: from.center.min(to.center),
                    end: from.center.max(to.center),
                    units: BroadcastUnits::Pixels,
                    reverse: from.center > to.center,
                    agent_type: from.agent_type,
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
    rail_ref: &NodeRef,
    rendered_sessions: &[(Uuid, AgentType)],
    axis: RailAxis,
    render_time: f64,
) -> Html {
    let views = measure_rendered_positions(rail_ref, rendered_sessions, axis)
        .map(|positions| broadcasts.view_for_positions(&positions, render_time))
        .unwrap_or_else(|| broadcasts.view_for_sessions(rendered_sessions, render_time));
    if views.is_empty() {
        return html! {};
    }

    html! {
        <div class={classes!("agent-broadcast-layer", axis.class())} aria-hidden="true">
            { for views.into_iter().map(|view| {
                let span = (view.end - view.start).max(1.0);
                let unit = view.units.css_unit();
                let style = match axis {
                    RailAxis::Horizontal => {
                        format!("left: {:.2}{unit}; width: {:.2}{unit};", view.start, span)
                    }
                    RailAxis::Vertical => {
                        format!("top: {:.2}{unit}; height: {:.2}{unit};", view.start, span)
                    }
                };
                let packet_class = match view.agent_type {
                    AgentType::Claude => "claude",
                    AgentType::Codex => "codex",
                };
                html! {
                    <span
                        key={format!("{}-{}-{:.0}", view.from_session_id, view.to_session_id, view.timestamp)}
                        class={classes!("agent-broadcast-path", view.reverse.then_some("reverse"))}
                        {style}
                    >
                        <span class={classes!("agent-broadcast-packet", packet_class)}>
                            <span class="agent-broadcast-packet-logo" />
                        </span>
                    </span>
                }
            }) }
        </div>
    }
}

impl BroadcastUnits {
    fn css_unit(self) -> &'static str {
        match self {
            Self::Percent => "%",
            Self::Pixels => "px",
        }
    }
}

fn measure_rendered_positions(
    rail_ref: &NodeRef,
    rendered_sessions: &[(Uuid, AgentType)],
    axis: RailAxis,
) -> Option<Vec<RenderedSessionPosition>> {
    let rail = rail_ref.cast::<Element>()?;
    let container = rail.parent_element()?;
    let container_rect = container.get_bounding_client_rect();
    let origin = match axis {
        RailAxis::Horizontal => container_rect.left(),
        RailAxis::Vertical => container_rect.top(),
    };

    let mut positions = Vec::with_capacity(rendered_sessions.len());
    for (id, agent_type) in rendered_sessions {
        let selector = format!("[data-session-id=\"{id}\"]");
        let pill = rail.query_selector(&selector).ok().flatten()?;
        let rect = pill.get_bounding_client_rect();
        let center = match axis {
            RailAxis::Horizontal => (rect.left() + rect.right()) / 2.0 - origin,
            RailAxis::Vertical => (rect.top() + rect.bottom()) / 2.0 - origin,
        };
        positions.push(RenderedSessionPosition {
            id: *id,
            agent_type: *agent_type,
            center,
        });
    }
    Some(positions)
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
        assert!((view[0].start - 16.666).abs() < 0.01);
        assert!((view[0].end - 83.333).abs() < 0.01);
        assert_eq!(view[0].units, BroadcastUnits::Percent);
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
    fn broadcast_view_uses_sender_agent_type_for_packet_logo() {
        let events = BroadcastRef::default();
        events.push(AgentMessageBroadcast {
            from_session_id: id(2),
            to_session_id: id(1),
            timestamp: 1_000.0,
        });

        let view = events.view_for_sessions(
            &[
                (id(1), AgentType::Claude),
                (id(2), AgentType::Codex),
                (id(3), AgentType::Claude),
            ],
            1_200.0,
        );

        assert_eq!(view.len(), 1);
        assert_eq!(view[0].agent_type, AgentType::Codex);
    }

    #[test]
    fn broadcast_view_uses_measured_pill_centers_when_available() {
        let events = BroadcastRef::default();
        events.push(AgentMessageBroadcast {
            from_session_id: id(3),
            to_session_id: id(1),
            timestamp: 1_000.0,
        });

        let view = events.view_for_positions(
            &[
                RenderedSessionPosition {
                    id: id(1),
                    agent_type: AgentType::Claude,
                    center: 120.0,
                },
                RenderedSessionPosition {
                    id: id(2),
                    agent_type: AgentType::Codex,
                    center: 310.0,
                },
                RenderedSessionPosition {
                    id: id(3),
                    agent_type: AgentType::Codex,
                    center: 575.0,
                },
            ],
            1_200.0,
        );

        assert_eq!(view.len(), 1);
        assert_eq!(view[0].start, 120.0);
        assert_eq!(view[0].end, 575.0);
        assert_eq!(view[0].units, BroadcastUnits::Pixels);
        assert!(view[0].reverse);
        assert_eq!(view[0].agent_type, AgentType::Codex);
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
