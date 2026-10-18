//! The agent conversation shown in the command palette.
//!
//! Every natural-language request, from the person at the palette or from
//! an external agent over the agent protocol, becomes a [`Turn`]: the
//! request, the steps the assistant planned, and how each step went. Steps
//! that wait for a launched app's window stay [`StepStatus::Waiting`] until
//! it maps, so the palette shows progress as it happens.
//!
//! A request the built-in assistant does not follow can go to a language
//! model instead ([`crate::model`]). Its turn starts with no steps and
//! gains one per tool the model calls, says what the model is doing while
//! it works, and ends with the model's answer.

use std::collections::VecDeque;

use crate::action::Action;

/// Turns kept; older ones are dropped.
pub const MAX_TURNS: usize = 50;

/// Who made a request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    /// The person, typing in the palette.
    User,
    /// An external agent over the agent protocol.
    Agent,
}

/// How one step went.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StepStatus {
    /// Applied.
    Done,
    /// Waits for a launched app's window.
    Waiting,
    /// Failed with this message.
    Failed(String),
    /// Not attempted because an earlier step failed.
    Skipped,
    /// Waits for the person to allow it: the model wants to click, type or
    /// change a setting.
    Asking,
}

/// One planned action and its outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Step {
    /// The action, when the step is one; a model's step that reads the
    /// desktop or clicks in an app has none.
    pub action: Option<Action>,
    /// A human description, from [`Action::label`].
    pub label: String,
    /// Outcome so far.
    pub status: StepStatus,
}

/// One request and what came of it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Turn {
    /// Increasing ID, for "seen" tracking.
    pub id: u64,
    /// Who asked.
    pub source: Source,
    /// What was asked.
    pub request: String,
    /// The steps, in order.
    pub steps: Vec<Step>,
    /// Set when the request was not understood, or the model failed.
    pub error: Option<String>,
    /// What the model is doing now ("Thinking…"); the turn is not settled
    /// while it is set.
    pub working: Option<String>,
    /// The model's answer, once it has one.
    pub answer: Option<String>,
}

impl Turn {
    /// A one-line summary of the outcome.
    pub fn reply(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self.steps.iter().any(|s| s.status == StepStatus::Asking) {
            return "Waiting for you to allow the step above…".to_owned();
        }
        if let Some(working) = &self.working {
            return working.clone();
        }
        if let Some(answer) = &self.answer {
            return answer.clone();
        }
        let failed = self.steps.iter().find_map(|s| match &s.status {
            StepStatus::Failed(e) => Some((s.label.as_str(), e.as_str())),
            _ => None,
        });
        if let Some((label, error)) = failed {
            return format!("Stopped at \"{label}\": {error}");
        }
        let waiting = self
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Waiting)
            .count();
        match waiting {
            0 => "Done.".to_owned(),
            1 => "Waiting for the app's window to finish 1 step…".to_owned(),
            n => format!("Waiting for the app's window to finish {n} steps…"),
        }
    }

    /// Whether every step finished (done or failed) and the model, if one
    /// answers this turn, has, so nothing is pending.
    pub fn is_settled(&self) -> bool {
        self.working.is_none()
            && self
                .steps
                .iter()
                .all(|s| !matches!(s.status, StepStatus::Waiting | StepStatus::Asking))
    }
}

/// A reference to one step of one turn.
pub type StepRef = (u64, usize);

/// Recent turns, oldest first.
#[derive(Clone, Debug, Default)]
pub struct Conversation {
    turns: VecDeque<Turn>,
    next_id: u64,
}

impl Conversation {
    /// Recent turns, oldest first.
    pub fn turns(&self) -> impl DoubleEndedIterator<Item = &Turn> {
        self.turns.iter()
    }

    /// The newest turn's ID, if any.
    pub fn last_id(&self) -> Option<u64> {
        self.turns.back().map(|t| t.id)
    }

    /// Starts a turn for `request` with planned `actions`, all waiting.
    pub fn start(&mut self, source: Source, request: &str, actions: &[Action]) -> u64 {
        self.next_id += 1;
        if self.turns.len() == MAX_TURNS {
            self.turns.pop_front();
        }
        self.turns.push_back(Turn {
            id: self.next_id,
            source,
            request: request.trim().to_owned(),
            steps: actions
                .iter()
                .map(|a| Step {
                    action: Some(a.clone()),
                    label: a.label(),
                    status: StepStatus::Waiting,
                })
                .collect(),
            error: None,
            working: None,
            answer: None,
        });
        self.next_id
    }

    /// Starts a turn the model answers, with no steps yet and `working`
    /// saying what it does first.
    pub fn start_model(&mut self, request: &str, working: &str) -> u64 {
        let id = self.start(Source::User, request, &[]);
        if let Some(turn) = self.turn_mut(id) {
            turn.working = Some(working.to_owned());
        }
        id
    }

    /// Adds a step to `turn`; `None` once the turn was dropped.
    pub fn push_step(
        &mut self,
        turn: u64,
        label: String,
        action: Option<Action>,
        status: StepStatus,
    ) -> Option<StepRef> {
        let t = self.turn_mut(turn)?;
        t.steps.push(Step {
            action,
            label,
            status,
        });
        Some((turn, t.steps.len() - 1))
    }

    /// Says what the model answering `turn` is doing now.
    pub fn set_working(&mut self, turn: u64, working: &str) {
        if let Some(t) = self.turn_mut(turn) {
            t.working = Some(working.to_owned());
        }
    }

    /// Ends `turn` with the model's answer, or why it failed.
    pub fn finish(&mut self, turn: u64, outcome: Result<String, String>) {
        if let Some(t) = self.turn_mut(turn) {
            t.working = None;
            match outcome {
                Ok(answer) => t.answer = Some(answer),
                Err(error) => t.error = Some(error),
            }
        }
    }

    /// One turn, by ID.
    pub fn turn(&self, id: u64) -> Option<&Turn> {
        self.turns.iter().find(|t| t.id == id)
    }

    fn turn_mut(&mut self, id: u64) -> Option<&mut Turn> {
        self.turns.iter_mut().find(|t| t.id == id)
    }

    /// Records a request that was not understood.
    pub fn not_understood(&mut self, source: Source, request: &str, error: &str) -> u64 {
        let id = self.start(source, request, &[]);
        if let Some(turn) = self.turns.back_mut() {
            turn.error = Some(error.to_owned());
        }
        id
    }

    /// Sets a step's status; unknown (dropped) turns are ignored.
    pub fn set(&mut self, (turn, step): StepRef, status: StepStatus) {
        if let Some(s) = self.turn_mut(turn).and_then(|t| t.steps.get_mut(step)) {
            s.status = status;
        }
    }

    /// Forgets every turn.
    pub fn clear(&mut self) {
        self.turns.clear();
    }
}
