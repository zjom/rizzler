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
        r#"(popup-show NAME WIDGET [OPTS])

Opens the overlay panel named NAME, or updates it in place if a popup
with that name is already on the stack. Either way, raises it to the
top.

NAME   — ident | str: reuse a name to update a popup's widget /
         placement / text without stacking; pick distinct names when you
         want several popups visible at once.
WIDGET — widget: the tree drawn inside the popup's outer rect, usually
         (w-block PROPS (w-popup-self)) for a buf-backed popup.
OPTS   — map: optional. Recognized keys, all optional:
           "text":         str — seed text for the backing buffer
                           (overwrites existing)
           "modes":        array of layer — keymap layers, specific last
           "mode":         layer — shorthand for "modes": [m]
           "buffer-mode":  mode — editing mode for the buffer
           "placement":    placement — see (placement-centered ...)
           "show-cursor":  truthy to draw the cursor over the buf
           "wrap-mode":    'none | 'char | 'word
           "wrap-column":  int — column to wrap at
           "break-indent": truthy to honor leading indentation on wrap

Returns bufno: the backing buffer's opaque id.

Example:
  (popup-show 'help
    (w-block {"border": "rounded" "title": " help "} (w-popup-self))
    {"text": "press q to dismiss"
     "modes": ['popup]
     "placement": (placement-centered 0.4 0.4)})

See also: (popup-hide NAME), (popup-close), (popup-visible? NAME)."#,
    );

    b.be_doc(
        "popup-hide",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "popup-hide.name")?;
            let closed = with_editor_mut(|st| st.hide_popup(&name));
            Ok(Rc::new(Value::Int(closed as i64)))
        },
        r#"(popup-hide NAME)

Closes the named overlay panel and frees its backing buffer. For the
"close whatever's on top" case, use (popup-close) instead.

NAME — ident | str: the popup to close.

Returns 1 if the popup was visible, else 0.

Example:
  (popup-hide 'help)
See also: (popup-show NAME WIDGET), (popup-close)."#,
    );

    b.be_doc(
        "popup-close",
        0,
        |_, _| {
            let closed = with_editor_mut(|st| st.close_popup());
            Ok(Rc::new(Value::Int(closed as i64)))
        },
        r#"(popup-close)

Closes the topmost overlay panel (skipping the minibuffer if it sits on
top). Useful for generic dismiss bindings like q / <esc> in the 'popup
keymap layer that shouldn't know which popup they're closing.

Returns 1 if a popup was closed, else 0.

Example:
  (keymap-set 'popup "q" '(popup-close))
See also: (popup-hide NAME), (popup?)."#,
    );

    b.be_doc(
        "popup-visible?",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "popup-visible?.name")?;
            let v = with_editor_mut(|st| st.popup_buf_by_name(&name).is_some());
            Ok(Rc::new(Value::Int(v as i64)))
        },
        r#"(popup-visible? NAME)

Returns 1 if a popup named NAME is currently on the stack, else 0.

NAME — ident | str: the popup to test.

Example:
  (if (popup-visible? 'help) (popup-hide 'help) (popup-show 'help ...))
See also: (popup?), (popup-show NAME WIDGET)."#,
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
        r#"(popup-bufno NAME)

Returns bufno: the backing buffer's opaque id for the named popup, or ()
if no popup with that name is visible. Feed it to (w-buffer-view BUFNO)
or (buffer-text-set BUFNO ...) for cross-popup queries.

NAME — ident | str: the popup to look up.

Example:
  (buffer-text-set (popup-bufno 'messages) new-text)
See also: (buffer-no), (minibuffer-bufno)."#,
    );

    b.be_doc(
        "minibuffer-bufno",
        0,
        |_, _| {
            let id = with_editor_mut(|st| st.minibuffer_id());
            Ok(Rc::new(Value::Int(buf_id_to_int(id))))
        },
        r#"(minibuffer-bufno)

Returns bufno: the minibuffer's stable opaque buffer id. Unlike popup
bufnos this never changes — there's exactly one minibuffer per editor.

Example:
  (w-buffer-view (minibuffer-bufno))
See also: (popup-bufno NAME), (buffer-no)."#,
    );

    b.be_doc(
        "popup-mode",
        0,
        |_, _| {
            let v =
                with_editor_mut(|st| st.top_popup_mode().map(Value::Str).unwrap_or(Value::Unit));
            Ok(Rc::new(v))
        },
        r#"(popup-mode)

Returns str: the topmost keymap layer of the topmost overlay panel, or
() if no popup is visible. Useful for "am I inside a popup of kind X?"
checks in shared helpers like (notify).

Example:
  (if (= (popup-mode) "popup") ... ...)
See also: (popup?), (popup-visible? NAME)."#,
    );

    b.be_doc(
        "popup?",
        0,
        |_, _| {
            let v = with_editor_mut(|st| st.has_popup());
            Ok(Rc::new(Value::Int(v as i64)))
        },
        r#"(popup?)

Returns 1 if any overlay panel is on the stack, else 0. Coarser than
(popup-visible? NAME) — useful for "is the editor currently obstructed"
checks that don't care which popup is up.

Example:
  (if (popup?) (popup-close) (set-mode 'command))
See also: (popup-visible? NAME), (popup-close)."#,
    );
}
