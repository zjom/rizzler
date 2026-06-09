//! Lisp runtime, notification queue, and journal hooks.
//!
//! The runtime is moved out of `State` for the duration of an eval (so editor
//! primitives can mutate `State` through the thread-local bridge without
//! aliasing `self.lisp`). Notifications fired by Rust during an outer eval are
//! queued and drained on the way out — see [`State::with_lisp`].

use std::rc::Rc;

use rizz::RizzError;
use rizz::runtime::Value;
use tracing::{debug, error, info, instrument, trace, warn};

use crate::lisp::{EditorGuard, LispRuntime};

use super::State;

impl State {
    /// Install an [`EditorGuard`] and run `f` against the editor's lisp
    /// runtime. On the way out, drain any notifications that were queued
    /// while the runtime was checked out (see [`Self::notify_via_lisp`]) —
    /// each one fires through the user's lisp `(notify …)` definition so the
    /// popup chrome stays under their control.
    pub(super) fn with_lisp<R>(&mut self, f: impl FnOnce(&mut LispRuntime) -> R) -> R {
        let mut lisp = self
            .lisp
            .take()
            .expect("recursive eval_lisp is not supported");
        let result = {
            let _guard = EditorGuard::new(self);
            f(&mut lisp)
        };
        self.lisp = Some(lisp);
        self.drain_pending_notifications();
        result
    }

    /// Fire every queued notification through the lisp `(notify …)` fn now
    /// that `self.lisp` is owned again. A cap keeps a buggy `notify`
    /// definition from looping forever — anything past the cap falls back
    /// to the message journal so it's still recoverable via `:messages`.
    pub(super) fn drain_pending_notifications(&mut self) {
        const MAX_DRAIN_PER_CALL: usize = 32;
        let mut drained = 0;
        while let Some(msg) = self.pending_notifications.pop() {
            if drained >= MAX_DRAIN_PER_CALL {
                warn!(
                    remaining = self.pending_notifications.len() + 1,
                    "notification drain cap hit — recording remainder to journal"
                );
                let remainder: Vec<String> = std::iter::once(msg)
                    .chain(self.pending_notifications.drain(..))
                    .collect();
                for m in remainder {
                    self.record_message(&m);
                }
                return;
            }
            drained += 1;
            self.notify_via_lisp(&msg);
        }
    }

    #[instrument(skip(self, src), fields(bytes = src.len()))]
    pub fn eval_lisp(&mut self, src: &str) -> Result<Rc<Value>, RizzError> {
        trace!(src = %src.chars().take(200).collect::<String>(), "eval_lisp src");
        let r = self.with_lisp(|lisp| lisp.eval_str(src));
        match &r {
            Ok(v) => trace!(result = %v.display(), "eval_lisp ok"),
            Err(e) => warn!(error = %e, "eval_lisp err"),
        }
        r
    }

    #[instrument(skip(self, form))]
    pub fn eval_lisp_value(&mut self, form: Rc<Value>) -> Result<Rc<Value>, RizzError> {
        trace!(form = %form.display(), "eval_lisp_value form");
        let r = self.with_lisp(|lisp| lisp.eval_value(form));
        if let Err(e) = &r {
            warn!(error = %e, "eval_lisp_value err");
        }
        r
    }

    #[instrument(skip(self, src), fields(bytes = src.len()))]
    pub fn eval_lisp_script(&mut self, src: &str) -> Result<(), RizzError> {
        let r = self.with_lisp(|lisp| lisp.eval_script(src));
        if let Err(e) = &r {
            warn!(error = %e, "eval_lisp_script err");
        }
        r
    }

    pub fn record_message(&mut self, msg: &str) {
        info!(target: "rizz::journal", msg, "journal: message");
        self.journal.record_message(msg);
    }

    pub fn message_history(&self) -> impl Iterator<Item = &Rc<str>> {
        self.journal.messages()
    }

    pub fn record_cmd(&mut self, msg: &str) {
        info!(target: "rizz::journal", cmd = msg, "journal: command");
        self.journal.record_command(msg);
        self.registers.record_command(msg);
    }
    pub fn cmd_history(&self) -> impl Iterator<Item = &Rc<str>> {
        self.journal.commands()
    }

    /// Bridge from Rust failure paths (eval errors, render-callback errors)
    /// to the lisp-side `notify` fn. Safe to call from inside a lisp builtin:
    /// when the runtime is checked out via `with_lisp`, the message is queued
    /// and drained on the way out — avoiding a recursive `lisp.take()` crash
    /// and the silent fallback to `record_message`.
    pub fn notify_via_lisp(&mut self, msg: &str) {
        debug!(msg, "notify_via_lisp");
        if self.lisp.is_none() {
            debug!("notify_via_lisp queued — lisp runtime checked out");
            self.pending_notifications.push(msg.to_string());
            return;
        }
        let src = format!("(notify {})", crate::lisp::quote_for_lisp(msg));
        if let Err(e) = self.eval_lisp(&src) {
            error!(error = %e, "notify-via-lisp failed");
            self.record_message(&format!("notify failed: {e}"));
        }
    }
}
