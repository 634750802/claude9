use anyhow::{bail, Result};
use serde_json::Value;

use crate::run9;

/// Resolve the snap id of a base box.
///
/// 1. If env var `CLAUDE9_BASE_SNAP_ID` is set (non-empty), use it directly.
///    This is the escape hatch for cases where `box inspect` can't give a
///    usable snap — e.g. the base box is running and its `box_snap_id` is
///    in state `inuse`, so you want to point at a pre-forked detached snap.
/// 2. `run9 box inspect <base_box>` → try a few candidate fields for the
///    snap id.
/// 3. Otherwise error with the raw inspect JSON so the user can see what's
///    there and either set the escape hatch or tell us which field to read.
pub fn resolve_base_snap(base_box: &str) -> Result<String> {
    if let Ok(id) = std::env::var("CLAUDE9_BASE_SNAP_ID") {
        if !id.is_empty() {
            return Ok(id);
        }
    }

    let view = run9::box_inspect(base_box)?;
    if let Some(id) = extract_snap_id(&view) {
        return Ok(id);
    }

    bail!(
        "could not find a snap id in `run9 box inspect {}` response. \
         Set CLAUDE9_BASE_SNAP_ID to bypass. Raw response:\n{}",
        base_box,
        serde_json::to_string_pretty(&view).unwrap_or_default()
    )
}

fn extract_snap_id(view: &Value) -> Option<String> {
    for key in ["box_snap_id", "snap_id", "source_snap_id"] {
        if let Some(s) = view.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}
