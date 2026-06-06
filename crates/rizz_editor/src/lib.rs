//! Top-level editor orchestration.
//!
//! - [`state::State`] owns every long-lived piece of the editor: the buffer
//!   list, the window tree, the keymap, the lisp runtime, the theme, popups,
//!   and the configured renderer. `State` is the single mutator — every
//!   action goes through [`State::apply`].
//! - [`lisp::LispRuntime`] embeds the `rizz` lisp language and registers
//!   every editor primitive as a native function. Builtins reach the live
//!   `State` through a thread-local pointer installed by [`lisp::EditorGuard`]
//!   only while a `State::eval_lisp*` call is on the stack.
//! - [`buffer_list::BufferList`] keeps the `Vec<Buffer>` invariants
//!   (minibuffer index re-syncs on removal).
//! - [`journal::Journal`] is the ring-buffer history surfaced by `:messages`
//!   / `:history`.
//!
//! A binary embedding the editor only needs to import [`State`] and
//! [`Config`], build a `State::with_config(Config::with_path(...))`, and call
//! `state.render()` + `state.handle_key_event(...)` in a loop.

pub mod buffer_list;
pub mod journal;
pub mod lisp;
pub mod state;

pub use buffer_list::{BufferList, CycleDir};
pub use journal::Journal;
pub use lisp::{EditorGuard, LispRuntime, init_script_path};
pub use state::{Config, PopupSpec, State};
