//! Parsers that translate the `(popup-open ...)` options map into a
//! [`PopupSpec`]. Split out from the builtin itself so the lisp/builtins
//! tree only handles dispatch — placement/dim wrangling lives here.

use std::rc::Rc;

use rizz::runtime::{RuntimeError, Value};

use rizz_text::wrap::WrapMode;
use rizz_ui::panel::{Dim, Placement, Side};

use super::helpers::{
    as_ident_or_str, as_int, as_str, parse_mode_ident, parse_mode_layers, parse_mode_name,
    unknown_variant,
};
use crate::state::PopupSpec;

pub(super) fn parse_popup_options(v: &Rc<Value>, spec: &mut PopupSpec) -> Result<(), RuntimeError> {
    let m = match &**v {
        Value::Unit => return Ok(()),
        Value::Map(m) => m,
        _ => {
            return Err(RuntimeError::type_mismatch(
                "popup-open.options",
                "map | ()",
                v,
            ));
        }
    };
    let key = |k: &str| Rc::new(Value::Str(k.into()));
    if let Some(t) = m.get(&key("text")) {
        spec.initial_text = Some(as_str(t, "popup-open.text")?.to_string());
    }
    if let Some(modes) = m.get(&key("modes")) {
        spec.mode_layers = parse_mode_layers(modes)?;
    } else if let Some(mode) = m.get(&key("mode")) {
        spec.mode_layers = vec![parse_mode_name(mode)?];
    }
    if let Some(bm) = m.get(&key("buffer-mode")) {
        spec.buffer_mode = parse_mode_ident(bm)?;
    }
    if let Some(p) = m.get(&key("placement")) {
        spec.placement = parse_placement(p)?;
    }
    if let Some(sc) = m.get(&key("show-cursor")) {
        spec.show_cursor = sc.is_truthy();
    }
    if let Some(sc) = m.get(&key("wrap-mode")) {
        spec.wrap_mode =
            WrapMode::from_str(&as_ident_or_str(sc, "popup-open.wrap-mode")?).unwrap_or_default();
    }
    if let Some(sc) = m.get(&key("wrap-column")) {
        spec.wrap_column = Some(as_int(sc, "popup-open.wrap-column")?.max(0) as u16);
    }
    if let Some(sc) = m.get(&key("break-indent")) {
        spec.breakindent = sc.is_truthy();
    }
    Ok(())
}

fn parse_placement(v: &Rc<Value>) -> Result<Placement, RuntimeError> {
    match &**v {
        Value::Ident(s) | Value::Str(s) => match s.as_ref() {
            "center" | "centered" => Ok(Placement::default()),
            "full" => Ok(Placement::Full),
            other => Err(unknown_variant("placement", other)),
        },
        Value::Map(m) => {
            let key = |k: &str| Rc::new(Value::Str(k.into()));
            let kind = m
                .get(&key("kind"))
                .map(|k| as_ident_or_str(k, "placement.kind"))
                .transpose()?
                .map(|s| s.to_string())
                .unwrap_or_else(|| "center".to_string());
            match kind.as_str() {
                "center" | "centered" => {
                    let width = m
                        .get(&key("w"))
                        .or_else(|| m.get(&key("width")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Frac(0.6));
                    let height = m
                        .get(&key("h"))
                        .or_else(|| m.get(&key("height")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Frac(0.6));
                    Ok(Placement::Centered { width, height })
                }
                "at" => {
                    let x = m
                        .get(&key("x"))
                        .map(|v| as_int(v, "placement.x"))
                        .transpose()?
                        .unwrap_or(0)
                        .max(0) as u16;
                    let y = m
                        .get(&key("y"))
                        .map(|v| as_int(v, "placement.y"))
                        .transpose()?
                        .unwrap_or(0)
                        .max(0) as u16;
                    let width = m
                        .get(&key("w"))
                        .or_else(|| m.get(&key("width")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Cells(40));
                    let height = m
                        .get(&key("h"))
                        .or_else(|| m.get(&key("height")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Cells(10));
                    Ok(Placement::At {
                        x,
                        y,
                        width,
                        height,
                    })
                }
                "side" => {
                    let side = m.get(&key("side")).ok_or_else(|| {
                        RuntimeError::type_mismatch("placement.side", "ident|str", v)
                    })?;
                    let side = match as_ident_or_str(side, "placement.side")?.as_ref() {
                        "top" => Side::Top,
                        "bottom" => Side::Bottom,
                        "left" => Side::Left,
                        "right" => Side::Right,
                        other => return Err(unknown_variant("placement.side", other)),
                    };
                    // Default to Fit: the popup sizes itself to the minimum
                    // rows/cols needed to contain the buffer's wrapped text.
                    let size = m
                        .get(&key("size"))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Fit);
                    Ok(Placement::Anchored { side, size })
                }
                "full" => Ok(Placement::Full),
                other => Err(unknown_variant("placement.kind", other)),
            }
        }
        _ => Err(RuntimeError::type_mismatch("placement", "ident|str|map", v)),
    }
}

fn parse_dim(v: &Rc<Value>) -> Result<Dim, RuntimeError> {
    match &**v {
        Value::Int(n) => Ok(Dim::Cells((*n).max(0) as u16)),
        Value::Float(f) => Ok(Dim::Frac(f.into_inner() as f32)),
        Value::Ident(s) | Value::Str(s) if s.as_ref() == "fit" => Ok(Dim::Fit),
        _ => Err(RuntimeError::type_mismatch("dim", "int|float|'fit", v)),
    }
}
