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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefers_box_snap_id() {
        let v = json!({ "box_snap_id": "sv1", "snap_id": "sv2" });
        assert_eq!(extract_snap_id(&v).as_deref(), Some("sv1"));
    }

    #[test]
    fn falls_back_through_candidate_keys() {
        let v = json!({ "snap_id": "sv2" });
        assert_eq!(extract_snap_id(&v).as_deref(), Some("sv2"));
        let v = json!({ "source_snap_id": "sv3" });
        assert_eq!(extract_snap_id(&v).as_deref(), Some("sv3"));
    }

    #[test]
    fn skips_empty_string_values() {
        // An empty string shouldn't count as "found" — fall through.
        let v = json!({ "box_snap_id": "", "snap_id": "sv2" });
        assert_eq!(extract_snap_id(&v).as_deref(), Some("sv2"));
    }

    #[test]
    fn returns_none_when_no_key_present() {
        let v = json!({ "other": 1 });
        assert_eq!(extract_snap_id(&v), None);
    }
}
