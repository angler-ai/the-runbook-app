//! One open session per object instance: the ExecutionDO's equivalent of
//! the row lock a Postgres session holds from begin to commit.
//!
//! A session's `begin` waits here until the previous session committed,
//! released or was dropped. Core relies on this serialization (a refresh
//! re-reads its token row after `begin`; a replayed request finds the
//! reply its twin committed), and it stops two actors that interleave at a
//! D1 or R2 await from both deciding on one snapshot. A Durable Object has
//! one instance and runs one thread, so a `Cell` and a waker queue are the
//! whole lock. Correctness does not rest on it alone: every session commit
//! also checks the `meta.writes` fence (plain commits do not take the gate).
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::poll_fn,
    rc::Rc,
    task::{Poll, Waker},
};

#[derive(Default)]
pub(crate) struct Gate {
    held: Cell<bool>,
    waiting: RefCell<VecDeque<Waker>>,
}

/// Holds the gate until dropped.
pub(crate) struct Held {
    state: Rc<crate::ObjectState>,
}
impl Drop for Held {
    fn drop(&mut self) {
        let gate = &self.state.gate;
        gate.held.set(false);
        if let Some(next) = gate.waiting.borrow_mut().pop_front() {
            next.wake();
        }
    }
}

/// Wait for the gate of `state`'s object.
pub(crate) async fn enter(state: Rc<crate::ObjectState>) -> Held {
    poll_fn(|cx| {
        let gate = &state.gate;
        if gate.held.get() {
            gate.waiting.borrow_mut().push_back(cx.waker().clone());
            Poll::Pending
        } else {
            gate.held.set(true);
            Poll::Ready(())
        }
    })
    .await;
    Held { state }
}
