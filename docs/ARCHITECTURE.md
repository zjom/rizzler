# Architecture

A reading guide for new developers. If you've just cloned the repo and want to know where things live, what owns what, and how a keystroke turns into a buffer change — start here.

The editor is a terminal-modal Rust binary (`rizzler`) with an embedded lisp runtime. User code in `init.rz` configures behaviour by registering native functions and binding keys; everything else is driven through one funnel.

## The single-funnel invariant

There is one rule that holds the system together. Read it first.

> Every behaviour the editor can perform is an [`Action`]. Every input source — keystrokes, lisp calls, LSP responses, paste events — ultimately produces an `Action` list and runs through [`State::apply`]. Adding a behaviour means adding a variant to the `Action` enum and a match arm in `apply()`.

This is what makes undo, scripting, tests, and reasoning tractable. Resist new entry points.

`Action` lives in [`crates/rizz_actions/src/action.rs`](../crates/rizz_actions/src/action.rs); `State::apply` lives in [`crates/rizz_editor/src/state/apply.rs`](../crates/rizz_editor/src/state/apply.rs).

## A keystroke, end to end

This is the path you'll be modifying or debugging most often. Read it once, then refer back when something behaves unexpectedly.

```
crossterm event loop                       (src/main.rs)
  └─ State::handle_key_event                (state/input.rs)
       ├─ ring buffer of recent keys        (for chord timeout)
       ├─ CountPrefix::feed                 (digits before a motion)
       └─ KeymapRegistry::resolve(modes, ke, timedout)
            ├─ active_modes()               (panel layers + buffer mode)
            └─ Action(s)                    (rizz_actions)
                 └─ State::apply            (state/apply.rs)
                      └─ buffer / window / lsp / lisp mutation
                            └─ refresh_viewport + render
```

The `modes` stack is "most specific first": if a popup is on top of the stack, its keymap layers come before the focused buffer's [`EditingMode`]. That's how `q` dismisses a popup but inserts a `q` in Insert mode without conflicting bindings.

For paste events, the keymap is bypassed entirely: `Event::Paste(text)` becomes `Action::InsertMany(text)` directly so embedded newlines don't get reparsed as `Ctrl+J`.

## `State`

[`State`](../crates/rizz_editor/src/state/mod.rs) is the editor process. It owns every long-lived field and is the single mutator. The struct is a thin facade — 13 top-level fields, most of them per-concern subsystem structs:

| Concern                | Field on `State`               | Subsystem fields                                                                                          | Lives in                                                                       |
| ---------------------- | ------------------------------ | --------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Buffer registry        | `bufs: BufferList`             | `bufs: SlotMap`, `file_order`, `minibuffer`, `by_uri`                                                     | [`buffer_list.rs`](../crates/rizz_editor/src/buffer_list.rs)                   |
| Surface (windows+UI)   | `surface: Surface`             | `windows: WindowTree`, `panels: PanelStack`                                                               | [`state/surface.rs`](../crates/rizz_editor/src/state/surface.rs)               |
| Key input              | `input: Input`                 | `keymap`, `keyevents`, `keycombo_timeout`, `count_prefix`                                                 | [`state/input.rs`](../crates/rizz_editor/src/state/input.rs)                   |
| Render config          | `render: Render`               | `renderer`, `theme`, `frame_fn`, `gutter_fn`, `gutter_width`                                              | [`state/render.rs`](../crates/rizz_editor/src/state/render.rs)                 |
| Scripting              | `scripting: Scripting`         | `lisp`, `pending_notifications`                                                                           | [`state/scripting.rs`](../crates/rizz_editor/src/state/scripting.rs)           |
| Workspace paths        | `workspace: Workspace`         | `workdir`, `config_dir`                                                                                   | [`state/workspace.rs`](../crates/rizz_editor/src/state/workspace.rs)           |
| Lang integration       | `lang: LangIntegration`        | `ts: LanguageBackend<GrammarSpec>`, `ts_registry`, `lsp: LanguageBackend<ServerSpec>`, `lsp_registry`     | [`state/lang.rs`](../crates/rizz_editor/src/state/lang.rs)                     |
| LSP session            | `lsp_session: LspSession`      | `pending_requests`, `next_seq`, `completion_fn`, `code_action_fn`, `pending_completion`, `pending_code_actions` | [`state/lsp_session.rs`](../crates/rizz_editor/src/state/lsp_session.rs) |
| Yank/paste registers   | `registers: Registers`, `pending_register: Option<char>` | —                                                                                       | yank-paste flows                                                               |
| Other top-level fields | `journal: Journal`, `search: Search`, `quit: bool`       | —                                                                                       | scripting / search / quit flag                                                 |

`State`'s methods live in sibling files under [`state/`](../crates/rizz_editor/src/state) — `mod.rs` declares each as a child module. Rust treats child modules as having full access to the parent's private fields, so every subsystem file can read and mutate `State`'s private state directly. This keeps the split mechanical: no `pub(crate)` field plumbing.

Each subsystem struct is a `pub(super)` plain-old-data grouping that's constructed once in `State::with_config`. Methods stay on `impl State` and reach in via `self.input.keymap`, `self.lang.ts.manifest`, etc. — keeping the public API on `State` stable while the field grouping documents what belongs together.

When `State::apply` arms need to touch fields from two or more subsystems at once (e.g. `Action::BufCreate` touches `bufs`, `lang`, `lsp_session`, `scripting`), prefer free functions that take field-disjoint `&mut` references over `&mut self` methods. The latter hits `E0499` when you split borrows; the former is the same pattern rustc itself uses (`rustc_borrowck`-style).

## The crate graph

There are ~15 workspace crates. The dependency tree is shallow and acyclic, with `rizz_editor` as the orchestration hub.

```
                        ┌──────────────┐
                        │  rizz_core   │  (Position, EditingMode, FocusDir, …)
                        └──┬───────────┘
                           │
    ┌──────────────────────┼────────────────────────────────┐
    │                      │                                │
rizz_input             rizz_text ──→ rizz_changetree   rizz_registers
    │                  rizz_text ──→ rizz_ts ──→ libloading
    │                      │                                │
    │                  rizz_search                          │
    │                      │                                │
    └─→ rizz_ui ←──────────┘                                │
          ▲                                                 │
          │                                                 │
    rizz_actions ───────────────────────────────────────────┘
          │
          ├─→ rizz_lsp ──→ rizz_lsp_install
          │                       │
          │              rizz_ts_install ───→ rizz_install
          │                       │
          └─→ rizz_editor ←───────┘
                  │
               rizzler  (binary in src/main.rs)
```

(Arrows point from consumer to dependency.)

- **`rizz_actions`** is the closed enum of every behaviour. It depends only on data crates (`rizz_core`, `rizz_text`, `rizz_input`, `rizz_registers`) — no UI or LSP transport — so it stays the universal currency between input sources and `State::apply`.
- **`rizz_install`** is shared installer plumbing (manifest parsing, ext index, cache helpers, `LanguageBackend<S>` workflow state). Both `rizz_ts_install` and `rizz_lsp_install` consume it.
- **`rizz_editor`** depends on most of the others. That's intentional — it's the orchestration crate; everything below is "subsystem code that knows its own job". If `rizz_editor` ever feels like it's doing too much, the right move is to grow a subsystem under [`state/`](../crates/rizz_editor/src/state), not to fork a new top-level crate.

## Language backends: the `LanguageBackend<S>` pattern

Tree-sitter grammars and LSP servers both follow the same workflow:

1. A curated TOML manifest maps `name → spec`.
2. Each spec lists file extensions; an ext-index reverses `ext → name`.
3. On buffer open, the auto-load hook looks up the name, asks the cache, and falls back to a one-shot install if `auto_install` is on.
4. One-shot "I've already warned the user about this" / "I've already tried to install this and failed" sets keep notifications from spamming.

[`rizz_install::LanguageBackend<S>`](../crates/rizz_install/src/lib.rs) is the editor-side state for (1), (3), and (4) — `Manifest<S>` plus the `auto_install: bool`, `warned_missing`, and `failed_auto_installs` sets. The install side effects (git+tree-sitter for grammars, shell recipe for LSP) stay separate in `rizz_ts_install` and `rizz_lsp_install`.

[`State::lang`](../crates/rizz_editor/src/state/lang.rs) holds two of these — `lang.ts: LanguageBackend<GrammarSpec>` and `lang.lsp: LanguageBackend<ServerSpec>` — plus the runtime registry handles (`TsRegistry`, `LspRegistry`). The `install_highlighter` and `install_lsp_client` flows are still parallel implementations, but the bookkeeping (`first_warn`, `mark_failed`, `already_failed`, `forget`) is shared.

## The lisp bridge

User scripts live in `init.rz` (seeded from the embedded template on first launch). The lisp runtime is `rizz` — a small embedded lisp registered with native fns that mutate `State`.

Two invariants make the bridge sound:

1. **Re-entrancy is forbidden.** `eval_lisp*` calls `with_lisp` which `.take()`s `self.lisp`. If a builtin tries to re-enter `eval_lisp`, the unwrap panics — but the bookkeeping handles the legitimate "Rust code wants to call `notify` during a lisp eval" path: see [`State::notify_via_lisp`](../crates/rizz_editor/src/state/scripting.rs). It queues into `pending_notifications` if the runtime is checked out and drains on the way out of `with_lisp`.
2. **Mutable access is RAII-guarded.** Native fns reach `&mut State` through a thread-local pointer installed by [`EditorGuard`](../crates/rizz_editor/src/lisp/mod.rs). The guard is alive iff some `State::eval_lisp*` call is on the stack with unique `&mut self`. While the guard is alive, builtins can call `with_editor_mut(|st| …)` for full mutable access. Outside the guard, `with_editor_mut` panics — that's the assertion that catches "lisp fn called outside an editor eval".

Render is also guarded: [`RenderPhaseGuard`](../crates/rizz_editor/src/lisp/mod.rs) flips a thread-local flag while `precompute_frame` walks the slot registry, and lisp builtins that would mutate buffer state error out — a render callback can't corrupt the in-flight frame.

The user-facing surface is in [`crates/rizz_editor/src/lisp/builtins/`](../crates/rizz_editor/src/lisp/builtins/) — ~20 modules, ~148 builtins. Each module owns a domain (text, motion, bufs, windows, keymap, popups, lsp, …) and registers its fns into a shared [`Builtins`](../crates/rizz_editor/src/lisp/helpers.rs) sink.

## Buffers

[`rizz_text::Buffer`](../crates/rizz_text/src/buffer/mod.rs) is a rope-backed editable buffer. It owns text, cursor, viewport, soft-wrap, undo (`ChangeTree`), syntax highlighting (`Highlighter`), and an LSP attachment handle (`Box<dyn LspBufferHandle>` — type-erased so `rizz_text` doesn't pull in async).

The submodules under `buffer/` already split it by concern (`cursor.rs`, `edits.rs`, `marks.rs`, `text_object.rs`, `yank.rs`, `lsp.rs`) but share `pub(crate)` access to the same fields.

Buffers are owned by [`BufferList`](../crates/rizz_editor/src/buffer_list.rs) — a `SlotMap` keyed by `BufferId`, with a parallel ordered list of _file_ buffers (the minibuffer and panel-backing buffers are not in the file cycle).

## Rendering

The render pass has two phases:

1. **Precompute** ([`State::precompute_frame`](../crates/rizz_editor/src/state/render.rs)) — invokes user lisp callbacks (`frame_fn`, `gutter_fn`) under an `EditorGuard` + `RenderPhaseGuard`. Builds a `RenderedFrame` describing what each window should look like. Mutating builtins error out during this phase.
2. **Renderer** ([`State::render`](../crates/rizz_editor/src/state/render.rs)) — hands the `RenderedFrame` plus a `StateSnapshot` to the [`Renderer`](../crates/rizz_ui/src/render.rs) trait (default impl: `RatatuiRenderer`). The renderer is the only place that touches the terminal output.

`set_frame_fn` / `set_gutter` configure the lisp callbacks. They're deliberately direct setters rather than Actions — they configure UI hooks, not editor state.

## LSP

The LSP integration is split between three pieces:

- [`rizz_lsp`](../crates/rizz_lsp/) — async runtime, codec, registry of spawned clients, event channel. No state knowledge.
- [`rizz_lsp_install`](../crates/rizz_lsp_install/) — manifest + cache + shell recipe runner. No runtime knowledge.
- `State::lang.lsp` (workflow) and `State::lsp_session` (in-flight requests, sequence counter, completion / code-action callbacks, pending batches) — editor-side bookkeeping. URI ↔ buffer routing for server-pushed notifications is owned by `State::bufs` (`BufferList::register_uri` / `id_for_uri` / `unregister_uris_for`), so a buffer carries its LSP URI with it for the lifetime of the attachment.

The request/response shape mirrors the LSP protocol: `Action::LspHover` etc. _request_, the runtime emits `LspEvent::HoverResponse`, [`State::tick`](../crates/rizz_editor/src/state/lsp_session.rs) drains the events and re-enters `apply` with response Actions like `Action::LspShowHover`. The request side and the response side are different Action variants on purpose — they cross an async boundary.

## Testing

`State::test_support::test_state()` constructs a `State` with a `NullRenderer` (no terminal). Most editor tests go through `state.apply(&actions)` or `state.handle_key_event(...)` and assert on observable state. Lisp builtins are tested through `state.eval_lisp("...")`.

There are no integration tests of the binary itself; if you need to exercise the live terminal path, use `cargo run -- /path/to/file`.

## Where to look when…

- _Something about a key binding breaks_: [`state/input.rs`](../crates/rizz_editor/src/state/input.rs) + [`crates/rizz_actions/src/keymap`](../crates/rizz_actions/src/keymap/) (the trie + descent), then init.rz for the user-side binding.
- _A lisp call panics or doesn't see updates_: [`crates/rizz_editor/src/lisp/mod.rs`](../crates/rizz_editor/src/lisp/mod.rs) (the RAII guards) and the relevant builtin in [`builtins/`](../crates/rizz_editor/src/lisp/builtins).
- _Buffer text math is off_: [`crates/rizz_text/src/buffer/edits.rs`](../crates/rizz_text/src/buffer/edits.rs) for inserts/deletes, [`cursor.rs`](../crates/rizz_text/src/buffer/cursor.rs) for motion, [`wrap.rs`](../crates/rizz_text/src/wrap.rs) for soft-wrap.
- _Render output looks wrong_: [`state/render.rs`](../crates/rizz_editor/src/state/render.rs) for the precompute pass, [`crates/rizz_ui/src/precompute.rs`](../crates/rizz_ui/src/precompute.rs) for the per-buffer walk, [`crates/rizz_ui/src/render_ratatui.rs`](../crates/rizz_ui/src/render_ratatui.rs) for the terminal-side conversion.
- _LSP response is dropped_: [`state/lsp_session.rs::handle_lsp_event`](../crates/rizz_editor/src/state/lsp_session.rs) — every `LspEvent` either becomes a response Action or short-circuits with a warn.
- _A grammar fails to auto-install_: `lang.ts` warn/failed sets clamp retries; clear them via `(grammar-install '<name>)` or `reload-config`.

## Conventions

- All mutations go through `Action::apply`. If you find yourself reaching for a side door, the test/undo story breaks.
- Lisp builtins should call methods on `State` (via `with_editor_mut`); not poke at fields directly through `State`'s public surface.
- New `Action` variants live in [`rizz_actions/src/action.rs`](../crates/rizz_actions/src/action.rs) with a doc comment that names the Vim/Emacs analogue when there is one.
- Tests for `State`-level behaviour live in [`crates/rizz_editor/src/state/tests.rs`](../crates/rizz_editor/src/state/tests.rs).
- The codebase intentionally has no `unsafe` outside the thread-local lisp bridge — see [`with_editor_mut`](../crates/rizz_editor/src/lisp/mod.rs) for the load-bearing safety comment.
