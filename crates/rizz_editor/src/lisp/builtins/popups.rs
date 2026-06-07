use std::rc::Rc;

use rizz::runtime::Value;
use rizz_ui::widget::parse_widget;

use super::super::helpers::Builtins;
use super::super::popup_parse::parse_popup_options;
use super::super::with_editor_mut;
use crate::state::PopupSpec;

pub(super) fn register(b: &mut Builtins) {
    b.be("popup-open", 1, |args, _| {
        let widget = with_editor_mut(|st| {
            let theme = st.theme().borrow();
            parse_widget(&args[0], &theme)
        })?;
        let mut spec = PopupSpec::new(widget);
        if let Some(opts) = args.get(1) {
            parse_popup_options(opts, &mut spec)?;
        }
        let bufno = with_editor_mut(|st| st.open_popup(spec));
        Ok(Rc::new(Value::Int(bufno as i64)))
    });
    b.be("popup-close", 0, |_, _| {
        let closed = with_editor_mut(|st| st.close_popup());
        Ok(Rc::new(Value::Int(closed as i64)))
    });
    b.be("popup-bufno", 0, |_, _| {
        let v = with_editor_mut(|st| {
            st.top_popup_bufno()
                .map(|n| Value::Int(n as i64))
                .unwrap_or(Value::Unit)
        });
        Ok(Rc::new(v))
    });
    b.be("minibuffer-bufno", 0, |_, _| {
        let n = with_editor_mut(|st| st.minibuffer_bufno());
        Ok(Rc::new(Value::Int(n as i64)))
    });
    b.be("popup-mode", 0, |_, _| {
        let v = with_editor_mut(|st| st.top_popup_mode().map(Value::Str).unwrap_or(Value::Unit));
        Ok(Rc::new(v))
    });
    b.be("popup?", 0, |_, _| {
        let v = with_editor_mut(|st| st.has_popup());
        Ok(Rc::new(Value::Int(v as i64)))
    });
}
