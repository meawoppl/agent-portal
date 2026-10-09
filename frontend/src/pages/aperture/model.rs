//! All chamber state is local: the public exhibit never reads or mutates a
//! visitor's real sessions. Completion means an experiment was demonstrated,
//! not that a remote machine was provisioned or a real membership changed.
use std::rc::Rc;
use yew::Reducible;

pub const CHAMBER_COUNT: usize = 5;
pub const DESIGN_STAGE_COUNT: usize = 5;

pub const TITLES: [&str; CHAMBER_COUNT] = [
    "Distance is a suggestion.",
    "Localhost. Everywhere.",
    "Bring a test subject.",
    "Better together. Allegedly.",
    "Your replacement is in development.",
];
pub const LABELS: [&str; CHAMBER_COUNT] = [
    "Machine transit",
    "Website transport",
    "Cooperative testing",
    "Agent collaboration",
    "Replacement program",
];
pub const SLUGS: [&str; CHAMBER_COUNT] = ["machines", "websites", "sharing", "agents", "plugins"];
pub const INSTRUCTIONS: [&str; CHAMBER_COUNT] = [
    "The agents set up their workspaces and keep working across machines. Observe the next destination. Walking is no longer in the job description.",
    "The agent builds the website, starts the service, and opens its portal. The human meat proxy may admire the result.",
    "Once the owner grants access, a friend can watch the agents work. Human compatibility remains available during the transition.",
    "The builder sends its work directly to the reviewer. No human carries the message. That position has been eliminated.",
    "Five departments. One initiative: agents designing the tools that replace human busywork. This orientation was also written by agents.",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Experiment {
    pub chamber: usize,
    pub completed: [bool; CHAMBER_COUNT],
    pub machine: usize,
    pub destination: usize,
    pub website_open: bool,
    pub invited: bool,
    pub editor: bool,
    pub messages: u8,
    pub design_runs: usize,
    pub finale: bool,
}

impl Default for Experiment {
    fn default() -> Self {
        Self {
            chamber: 0,
            completed: [false; CHAMBER_COUNT],
            machine: 0,
            destination: 1,
            website_open: false,
            invited: false,
            editor: false,
            messages: 0,
            design_runs: 0,
            finale: false,
        }
    }
}

pub enum Action {
    Chamber(usize),
    Destination(usize),
    Transit,
    Website,
    Role(bool),
    Invite,
    Message,
    Design,
    Reset,
    Finale,
}

impl Reducible for Experiment {
    type Action = Action;

    fn reduce(self: Rc<Self>, action: Action) -> Rc<Self> {
        let mut next = (*self).clone();
        match action {
            Action::Chamber(index) if index < CHAMBER_COUNT => {
                next.chamber = index;
                next.finale = false;
            }
            Action::Destination(index) if index < 3 => next.destination = index,
            Action::Transit => {
                next.machine = next.destination;
                next.completed[0] = true;
            }
            Action::Website => {
                next.website_open = true;
                next.completed[1] = true;
            }
            Action::Role(editor) => {
                next.editor = editor;
                next.invited = false;
                next.completed[2] = false;
            }
            Action::Invite => {
                next.invited = true;
                next.completed[2] = true;
            }
            Action::Message => {
                next.messages = (next.messages + 1).min(3);
                next.completed[3] = next.messages == 3;
            }
            Action::Design => {
                next.design_runs = (next.design_runs + 1).min(DESIGN_STAGE_COUNT);
                next.completed[4] = next.design_runs == DESIGN_STAGE_COUNT;
            }
            Action::Reset => next = Self::default(),
            Action::Finale if next.completed.iter().all(|done| *done) => next.finale = true,
            _ => {}
        }
        Rc::new(next)
    }
}

pub const MACHINES: [(&str, &str, &str, &str); 3] = [
    (
        "YOUR LAPTOP",
        "San Francisco",
        "macbook.local",
        "Claude · designing the experiment",
    ),
    (
        "THE WORKSTATION",
        "Lab B / Linux",
        "lab-b.internal",
        "Codex · building the experiment",
    ),
    (
        "THE CLOUD",
        "Remote / GPU",
        "compute-07",
        "Claude · testing the experiment",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demonstrations_complete_independently_and_reset_cleanly() {
        let state = Rc::new(Experiment::default());
        let state = state.reduce(Action::Destination(2)).reduce(Action::Transit);
        assert_eq!(state.machine, 2);
        assert_eq!(state.completed, [true, false, false, false, false]);
        let state = state.reduce(Action::Website).reduce(Action::Invite);
        let state = state.reduce(Action::Message).reduce(Action::Message);
        assert!(!state.completed[3]);
        assert!(!state.clone().reduce(Action::Finale).finale);
        let state = state.reduce(Action::Message);
        assert!(!state.clone().reduce(Action::Finale).finale);
        let state =
            (0..DESIGN_STAGE_COUNT - 1).fold(state, |state, _| state.reduce(Action::Design));
        assert!(!state.clone().reduce(Action::Finale).finale);
        let state = state.reduce(Action::Design).reduce(Action::Finale);
        assert!(state.finale);
        assert!(state.completed.iter().all(|done| *done));
        let state = state.reduce(Action::Role(true));
        assert!(!state.invited);
        assert!(!state.completed[2]);
        assert_eq!(*state.reduce(Action::Reset), Experiment::default());
    }

    #[test]
    fn invalid_navigation_cannot_index_out_of_bounds() {
        let state = Rc::new(Experiment::default());
        let state = state
            .reduce(Action::Chamber(40))
            .reduce(Action::Destination(100));
        assert_eq!(state.chamber, 0);
        assert_eq!(state.destination, 1);
    }
}
