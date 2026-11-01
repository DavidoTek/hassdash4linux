//! Entity states and the decoder for `subscribe_entities` messages.
//!
//! Compressed format sent by Home Assistant:
//! * `a` (added): `{entity_id: {"s": state, "a": attributes, "lc": last_changed, ...}}`
//! * `c` (changed): `{entity_id: {"+": {"s"?, "a"?, "lc"?, ...}, "-": {"a": [removed attribute names]}}}`
//! * `r` (removed): `[entity_id, ...]`

use std::collections::HashMap;

use serde_json::{Map, Value};

/// The state of one entity.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EntityState {
    pub entity_id: String,
    pub state: String,
    pub attributes: Map<String, Value>,
    /// Seconds since the epoch, if known.
    pub last_changed: Option<f64>,
}

/// Maximum accepted length of a state string.
const MAX_STATE: usize = 1024;

impl EntityState {
    /// Parse the full format used by `get_states`.
    pub fn from_full(v: &Value) -> Option<Self> {
        let entity_id = v.get("entity_id")?.as_str()?;
        if !hd_core::entry::is_valid_entity_id(entity_id) {
            return None;
        }
        Some(Self {
            entity_id: entity_id.to_owned(),
            state: clamp(v.get("state").and_then(Value::as_str).unwrap_or("")),
            attributes: v
                .get("attributes")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            last_changed: None,
        })
    }

    pub fn domain(&self) -> &str {
        hd_core::entry::domain_of(&self.entity_id)
    }

    /// The friendly name, falling back to the entity ID.
    pub fn friendly_name(&self) -> String {
        self.attr_str("friendly_name")
            .filter(|s| !s.trim().is_empty())
            .map(|s| hd_core::truncate_chars(s, 200))
            .unwrap_or_else(|| self.entity_id.clone())
    }

    pub fn attr_str(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).and_then(Value::as_str)
    }

    pub fn attr_f64(&self, key: &str) -> Option<f64> {
        self.attributes
            .get(key)
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite())
    }

    pub fn is_unavailable(&self) -> bool {
        self.state == "unavailable" || self.state == "unknown" || self.state.is_empty()
    }

    pub fn is_on(&self) -> bool {
        self.state == "on"
    }
}

fn clamp(s: &str) -> String {
    if s.len() > MAX_STATE {
        hd_core::truncate_chars(s, 255)
    } else {
        s.to_owned()
    }
}

/// Live entity states maintained from `subscribe_entities` events.
#[derive(Debug, Default, Clone)]
pub struct EntityCache {
    map: HashMap<String, EntityState>,
}

impl EntityCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, entity_id: &str) -> Option<&EntityState> {
        self.map.get(entity_id)
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Apply one `subscribe_entities` event. Returns the changed entity IDs.
    pub fn apply(&mut self, ev: &Value) -> Vec<String> {
        let mut changed = Vec::new();
        if let Some(added) = ev.get("a").and_then(Value::as_object) {
            for (id, v) in added {
                if !hd_core::entry::is_valid_entity_id(id) {
                    continue;
                }
                let st = EntityState {
                    entity_id: id.clone(),
                    state: clamp(v.get("s").and_then(Value::as_str).unwrap_or("")),
                    attributes: v
                        .get("a")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default(),
                    last_changed: v.get("lc").and_then(Value::as_f64),
                };
                self.map.insert(id.clone(), st);
                changed.push(id.clone());
            }
        }
        if let Some(diffs) = ev.get("c").and_then(Value::as_object) {
            for (id, d) in diffs {
                let Some(st) = self.map.get_mut(id) else {
                    continue;
                };
                if let Some(plus) = d.get("+") {
                    if let Some(s) = plus.get("s").and_then(Value::as_str) {
                        st.state = clamp(s);
                    }
                    if let Some(attrs) = plus.get("a").and_then(Value::as_object) {
                        for (k, v) in attrs {
                            st.attributes.insert(k.clone(), v.clone());
                        }
                    }
                    if let Some(lc) = plus.get("lc").and_then(Value::as_f64) {
                        st.last_changed = Some(lc);
                    }
                }
                if let Some(removed) = d
                    .get("-")
                    .and_then(|m| m.get("a"))
                    .and_then(Value::as_array)
                {
                    for k in removed.iter().filter_map(Value::as_str) {
                        st.attributes.remove(k);
                    }
                }
                changed.push(id.clone());
            }
        }
        if let Some(removed) = ev.get("r").and_then(Value::as_array) {
            for id in removed.iter().filter_map(Value::as_str) {
                if self.map.remove(id).is_some() {
                    changed.push(id.to_owned());
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn add_change_remove() {
        let mut c = EntityCache::new();
        let ch = c.apply(&json!({"a": {"light.k": {"s": "off", "a": {"friendly_name": "Kitchen", "brightness": null}, "c": "x", "lc": 1.5}}}));
        assert_eq!(ch, vec!["light.k"]);
        assert_eq!(c.get("light.k").unwrap().friendly_name(), "Kitchen");
        c.apply(
            &json!({"c": {"light.k": {"+": {"s": "on", "a": {"brightness": 128}, "lc": 2.0}}}}),
        );
        let s = c.get("light.k").unwrap();
        assert!(s.is_on());
        assert_eq!(s.attr_f64("brightness"), Some(128.0));
        assert_eq!(s.last_changed, Some(2.0));
        c.apply(&json!({"c": {"light.k": {"-": {"a": ["brightness"]}}}}));
        assert!(
            c.get("light.k")
                .unwrap()
                .attributes
                .get("brightness")
                .is_none()
        );
        let ch = c.apply(&json!({"r": ["light.k"]}));
        assert_eq!(ch, vec!["light.k"]);
        assert!(c.is_empty());
    }

    #[test]
    fn ignores_garbage() {
        let mut c = EntityCache::new();
        assert!(c.apply(&json!({"a": {"NOT VALID": {"s": "x"}}})).is_empty());
        assert!(
            c.apply(&json!({"c": {"light.unknown": {"+": {"s": "on"}}}}))
                .is_empty()
        );
        assert!(c.apply(&json!(42)).is_empty());
        assert!(c.apply(&json!({"a": 5, "c": [], "r": {}})).is_empty());
        let long = "x".repeat(5000);
        c.apply(&json!({"a": {"sensor.x": {"s": long}}}));
        assert!(c.get("sensor.x").unwrap().state.chars().count() <= 255);
    }

    #[test]
    fn full_format() {
        let s = EntityState::from_full(
            &json!({"entity_id": "scene.a", "state": "2024", "attributes": {"friendly_name": "A"}}),
        )
        .unwrap();
        assert_eq!(s.friendly_name(), "A");
        assert!(EntityState::from_full(&json!({"entity_id": "bad"})).is_none());
    }
}
