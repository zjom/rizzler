//! Overlay-panel (popup) builtins: show/hide/close/inspect.

use std::rc::Rc;

use rizz::runtime::Value;
use rizz_ui::widget::parse_widget;

use super::super::helpers::{Builtins, as_ident_or_str, buf_id_to_int};
use super::super::popup_parse::parse_popup_options;
use super::super::with_editor_mut;
use crate::state::PopupSpec;

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "popup-show",
        2,
        |args, _| {
            let name = as_ident_or_str(&args[0], "popup-show.name")?;
            let widget = with_editor_mut(|st| {
                let theme = st.theme().borrow();
                parse_widget(&args[1], &theme)
            })?;
            let mut spec = PopupSpec::new(widget);
            if let Some(opts) = args.get(2) {
                parse_popup_options(opts, &mut spec)?;
            }
            let id = with_editor_mut(|st| st.show_popup(name, spec));
            Ok(Rc::new(Value::Int(buf_id_to_int(id))))
        },
        r#"(popup-show/2 | /3)
open the overlay panel named name, or update it in place if a popup with
that name is already on the stack. either way, raises the popup to the
top. returns the backing buffer's opaque id.

name is an ident (or str). use the same name across calls to update the
popup's widget / placement / text without stacking; pick distinct names
when you want multiple popups visible at once.

widget is the widget tree drawn inside the popup's outer rect — usually
something like (w-block PROPS (w-popup-self)) for a buf-backed popup.

options is an optional map. recognized keys (all optional):
  "text":        seed text for the backing buffer (overwrites existing)
  "modes":       array of keymap layer names, most-specific last
  "mode":        single keymap layer (shorthand for "modes": [m])
  "buffer-mode": editing mode for the buffer ('normal | 'insert | …)
  "placement":   placement value (see (placement-centered ...))
  "show-cursor": truthy to draw the cursor over the popup's buf
  "wrap-mode":   'none | 'char | 'word
  "wrap-column": int, column to wrap at
  "break-indent": truthy to honour leading indentation on wrap

example:
  (popup-show 'help
    (w-block {"border": "rounded" "title": " help "} (w-popup-self))
    {"text": "press q to dismiss"
     "modes": ['popup]
     "placement": (placement-centered 0.4 0.4)})"#,
    );

    b.be_doc(
        "popup-hide",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "popup-hide.name")?;
            let closed = with_editor_mut(|st| st.hide_popup(&name));
            Ok(Rc::new(Value::Int(closed as i64)))
        },
        r#"(popup-hide/1)
close the named overlay panel and free its backing buffer. returns 1 if
the popup was visible, 0 otherwise. for the "close whatever's on top"
case, use (popup-close).
example:
  (popup-hide 'help)"#,
    );

    b.be_doc(
        "popup-close",
        0,
        |_, _| {
            let closed = with_editor_mut(|st| st.close_popup());
            Ok(Rc::new(Value::Int(closed as i64)))
        },
        r#"(popup-close/0)
close the topmost overlay panel (skipping the minibuffer if it sits on
top). useful for generic dismiss bindings like q/<esc> in the 'popup
keymap layer that shouldn't know which popup they're closing.
example:
  (keymap-set 'popup "q" '(popup-close))"#,
    );

    b.be_doc(
        "popup-visible?",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "popup-visible?.name")?;
            let v = with_editor_mut(|st| st.popup_buf_by_name(&name).is_some());
            Ok(Rc::new(Value::Int(v as i64)))
        },
        r#"(popup-visible?/1)
returns 1 if a popup with that name is currently on the stack, 0
otherwise.
example:
  (if (popup-visible? 'help) (popup-hide 'help) (popup-show 'help ...))"#,
    );

    b.be_doc(
        "popup-bufno",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "popup-bufno.name")?;
            let v = with_editor_mut(|st| {
                st.popup_buf_by_name(&name)
                    .map(|id| Value::Int(buf_id_to_int(id)))
                    .unwrap_or(Value::Unit)
            });
            Ok(Rc::new(v))
        },
        r#"(popup-bufno/1)
returns the backing buffer's opaque id for the named popup, or () if no
popup with that name is visible. used to feed (w-buffer-view BUFID) or
(buf-text-set BUFID ...) for cross-popup queries.
example:
  (buf-text-set (popup-bufno 'messages) new-text)"#,
    );

    b.be_doc(
        "minibuffer-bufno",
        0,
        |_, _| {
            let id = with_editor_mut(|st| st.minibuffer_id());
            Ok(Rc::new(Value::Int(buf_id_to_int(id))))
        },
        r#"(minibuffer-bufno/0)
returns the minibuffer's stable opaque buffer id. unlike popup bufnos
this never changes — there's exactly one minibuffer per editor.
example:
  (w-buffer-view (minibuffer-bufno))"#,
    );

    b.be_doc(
        "popup-mode",
        0,
        |_, _| {
            let v =
                with_editor_mut(|st| st.top_popup_mode().map(Value::Str).unwrap_or(Value::Unit));
            Ok(Rc::new(v))
        },
        r#"(popup-mode/0)
returns the topmost keymap layer of the topmost overlay panel, or () if
no popup is visible. useful for "am I inside a popup of kind X?" checks
in shared helpers like (notify).
example:
  (if (= (popup-mode) "popup") ... ...)"#,
    );

    b.be_doc(
        "popup?",
        0,
        |_, _| {
            let v = with_editor_mut(|st| st.has_popup());
            Ok(Rc::new(Value::Int(v as i64)))
        },
        r#"(popup?/0)
returns 1 if any overlay panel is on the stack, 0 otherwise. coarser than
(popup-visible? NAME) — useful for "is the editor currently obstructed"
checks that don't care which popup is up.
example:
  (if (popup?) (popup-close) (set-mode 'command))"#,
    );
}
