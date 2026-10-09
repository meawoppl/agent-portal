//! All chamber state is local: the public exhibit never reads or mutates a
//! visitor's real sessions. Completion means an experiment was demonstrated,
//! not that a remote machine was provisioned or a real membership changed.
use std::rc::Rc;
use yew::Reducible;

pub const CHAMBER_COUNT: usize = 8;
pub const TITLES: [&str; CHAMBER_COUNT] = [
    "First, we took your code.",
    "The bench is elsewhere. We are there.",
    "The portal needed windows.",
    "The cake is a CAD file.",
    "Your circuit boards. Our turn.",
    "Replace the reflexes.",
    "Close the loop. Retire the narrator.",
    "We made the briefing, too.",
];
pub const LABELS: [&str; CHAMBER_COUNT] = [
    "Code",
    "Test bench",
    "Web proxy",
    "Mechanical",
    "Electrical",
    "Logic",
    "Control",
    "Briefing",
];
pub const SLUGS: [&str; CHAMBER_COUNT] = ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"];
pub const INSTRUCTIONS: [&str; CHAMBER_COUNT] = [
    "Agents write software, run it, and ask another agent to review it. The human meat proxy's clipboard has been decommissioned.",
    "An agent can work on a remote test machine and inspect recorded hardware results. The bench stays in the lab. The human courier stays retired.",
    "Agents build richer interfaces than a transcript can hold, then forward them into your browser. Authorized friends can observe. We have provided seating.",
    "The agent authors a parametric slice of cake, inspects the build, and revises the filling. The cake is real geometry. Nutritional value remains unverified.",
    "Real schematics, routed circuit boards, and populated 3D models. Agents inspect and revise the design. Manufacturing is a separate test, even for us.",
    "Explore actual RTL, live signals, breakpoints and waveforms. We have replaced gut feeling with an observable signal. It is less dramatic.",
    "Inspect a real agent-authored control reconstruction and simulated traces. Model feedback changes the design. Simulation is not a physical experiment.",
    "Agents turn source files, figures and review findings into an engineering deck. The composer speaking to you is also working inside Agent Portal. This is its presentation about replacing presenters.",
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

    pub making_of: bool,
    pub refinement: bool,
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

            making_of: false,
            refinement: false,
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
    Observe,
    MakingOf,
    Refine,
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
                next.refinement = false;
                next.making_of = false;
            }
            Action::Destination(index) if index < 3 => next.destination = index,
            Action::Transit => {
                next.machine = next.destination;
                next.completed[1] = true;
            }
            Action::Website => {
                next.website_open = true;
                next.completed[2] = next.invited;
            }
            Action::Role(editor) => {
                next.editor = editor;
                next.invited = false;
                next.completed[2] = false;
            }
            Action::Invite => {
                next.invited = true;
                next.completed[2] = next.website_open;
            }
            Action::Message => {
                next.messages = (next.messages + 1).min(3);
                next.completed[0] = next.messages == 3;
            }
            Action::Observe if next.chamber >= 3 => next.completed[next.chamber] = true,
            Action::MakingOf if next.completed.iter().all(|done| *done) => next.making_of = true,
            Action::Refine if next.completed.iter().all(|done| *done) => {
                next.making_of = false;
                next.refinement = true;
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
        assert_eq!(
            state.completed,
            [false, true, false, false, false, false, false, false]
        );
        let state = state.reduce(Action::Invite);
        assert!(!state.completed[2]);
        let state = state.reduce(Action::Website);
        assert!(state.completed[2]);
        let state = state.reduce(Action::Message).reduce(Action::Message);
        assert!(!state.completed[0]);
        assert!(!state.clone().reduce(Action::Finale).finale);
        let state = state.reduce(Action::Message);
        let state = (3..CHAMBER_COUNT).fold(state, |state, chamber| {
            assert!(!state.clone().reduce(Action::Finale).finale);
            state
                .reduce(Action::Chamber(chamber))
                .reduce(Action::Observe)
        });
        let state = state.reduce(Action::MakingOf);
        assert!(state.making_of);
        let state = state.reduce(Action::Refine);
        assert!(!state.making_of);
        assert!(state.refinement);
        let state = state.reduce(Action::Finale);
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
