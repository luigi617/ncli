use std::collections::BTreeMap;
use std::mem;
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use super::sys::Group;

#[derive(Clone, Default)]
pub struct Cancellation(Arc<Node>);

#[derive(Default)]
struct Node {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    cancelled: bool,
    next_id: u64,
    running: BTreeMap<u64, Arc<Group>>,
    children: Vec<Weak<Node>>,
}

impl Cancellation {
    pub fn global() -> &'static Self {
        static GLOBAL: LazyLock<Cancellation> = LazyLock::new(Cancellation::default);
        &GLOBAL
    }

    pub fn child(&self) -> Self {
        let child = Self::default();
        let mut state = self.state();
        child.state().cancelled = state.cancelled;
        state.children.retain(|node| node.strong_count() > 0);
        state.children.push(Arc::downgrade(&child.0));
        child
    }

    pub fn cancel(&self) {
        let children = {
            let mut state = self.state();
            state.cancelled = true;
            for group in state.running.values() {
                group.kill();
            }
            self.0.changed.notify_all();
            mem::take(&mut state.children)
        };
        for node in children.iter().filter_map(Weak::upgrade) {
            Self(node).cancel();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.state().cancelled
    }

    pub(super) fn sleep(&self, delay: Duration) -> bool {
        let (state, _) = self
            .0
            .changed
            .wait_timeout_while(self.state(), delay, |state| !state.cancelled)
            .unwrap_or_else(PoisonError::into_inner);
        state.cancelled
    }

    pub(super) fn register(&self, group: Arc<Group>) -> Registration<'_> {
        let mut state = self.state();
        if state.cancelled {
            group.kill();
        }
        let id = state.next_id;
        state.next_id += 1;
        state.running.insert(id, group);
        Registration {
            cancellation: self,
            id,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(super) struct Registration<'a> {
    cancellation: &'a Cancellation,
    id: u64,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.cancellation.state().running.remove(&self.id);
    }
}
