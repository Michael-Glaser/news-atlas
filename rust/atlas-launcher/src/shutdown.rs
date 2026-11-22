//! Shutting down in the right order: the window, then the agent, then the model server.
//!
//! Order is not a detail. The window is the only thing that shows anything, and it is
//! the only thing that talks to the model: stopping the model while the window is
//! still drawing would leave a window whose Chat page asks a port nothing answers.
//! So the window closes first — it is closed by the user, and this step only reaps it
//! — and the model server child is stopped last.
//!
//! The agent sits between the two because it *works*: it claims a request and drives a
//! research pass through the model, so stopping the model under it would turn every
//! action it is on into a connection error. The agent is asked first (`SIGTERM`, which
//! it answers by finishing the line it is printing and exiting 0) and the model server
//! goes after it.
//!
//! The order is a constant here, not a property of the order the caller happened to
//! register steps in, so a later edit cannot quietly stop the model first. A step that
//! was not registered is simply skipped (a boot that found a model server already
//! answering adopted no child, and there is nothing of ours to stop).
//!
//! Before this the list was `Web, Browser, Model`: a listener in this process, a
//! headless Chrome, then the model. Two of those are gone from the product — the
//! window is a child process, it listens for nothing and it starts no browser — and the
//! monitoring agent was added, so the list has three entries, and the one that matters
//! (the model child is never left running) is unchanged.

use std::collections::BTreeMap;

/// One thing that has to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// The native window this process started (`atlas-desktop`).
    Window,
    /// The monitoring agent this process started (`atlas-monitor serve`).
    Agent,
    /// The model server child.
    Model,
}

impl Step {
    /// The word a user reads.
    pub fn name(&self) -> &'static str {
        match self {
            Step::Window => "window",
            Step::Agent => "agent",
            Step::Model => "model server",
        }
    }
}

/// The one shutdown order.
pub const ORDER: [Step; 3] = [Step::Window, Step::Agent, Step::Model];

/// A set of stop actions, run in [`ORDER`].
pub struct Shutdown {
    steps: BTreeMap<Step, Box<dyn FnMut() + Send>>,
}

impl Default for Shutdown {
    fn default() -> Shutdown {
        Shutdown::new()
    }
}

impl Shutdown {
    pub fn new() -> Shutdown {
        Shutdown {
            steps: BTreeMap::new(),
        }
    }

    /// Register what stopping one step means.
    pub fn add(mut self, step: Step, stop: impl FnMut() + Send + 'static) -> Shutdown {
        self.steps.insert(step, Box::new(stop));
        self
    }

    /// Run every registered step, in [`ORDER`], and return the steps that were
    /// actually performed (in the order they were performed).
    pub fn run(mut self) -> Vec<Step> {
        let mut performed: Vec<Step> = Vec::new();
        for step in ORDER {
            if let Some(mut stop) = self.steps.remove(&step) {
                stop();
                performed.push(step);
            }
        }
        performed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn steps_run_in_the_declared_order_whatever_order_they_were_registered_in() {
        let log = Arc::new(Mutex::new(Vec::<Step>::new()));
        let mut shutdown = Shutdown::new();
        for step in [Step::Model, Step::Window, Step::Agent] {
            let log = log.clone();
            shutdown = shutdown.add(step, move || log.lock().unwrap().push(step));
        }
        let performed = shutdown.run();
        assert_eq!(performed, ORDER.to_vec());
        assert_eq!(*log.lock().unwrap(), ORDER.to_vec());
    }

    #[test]
    fn a_step_that_was_never_registered_is_skipped_and_not_a_failure() {
        let log = Arc::new(Mutex::new(Vec::<Step>::new()));
        let log_clone = log.clone();
        let performed = Shutdown::new()
            .add(Step::Model, move || log_clone.lock().unwrap().push(Step::Model))
            .run();
        assert_eq!(performed, vec![Step::Model]);
        assert_eq!(*log.lock().unwrap(), vec![Step::Model]);
    }

    #[test]
    fn the_model_server_is_always_stopped_last() {
        // The order as a fact about the constant, so a reordering has to change this
        // line and think about it: window, then the agent that is using the model, then
        // the model itself.
        assert_eq!(ORDER.len(), 3);
        assert_eq!(ORDER[0], Step::Window);
        assert_eq!(ORDER[1], Step::Agent);
        assert_eq!(ORDER[2], Step::Model);
        let names: Vec<&str> = ORDER.iter().map(|step| step.name()).collect();
        assert_eq!(names, vec!["window", "agent", "model server"]);
    }

    #[test]
    fn every_step_is_run_exactly_once() {
        let count = Arc::new(Mutex::new(0u32));
        let mut shutdown = Shutdown::new();
        for step in ORDER {
            let count = count.clone();
            shutdown = shutdown.add(step, move || *count.lock().unwrap() += 1);
        }
        let performed = shutdown.run();
        assert_eq!(performed.len(), ORDER.len());
        assert_eq!(*count.lock().unwrap(), ORDER.len() as u32);
        // Running it again is not possible (the value is consumed); a second
        // `Shutdown::new()` starts from nothing, which is the property the boot needs.
        assert!(Shutdown::new().run().is_empty());
    }
}
