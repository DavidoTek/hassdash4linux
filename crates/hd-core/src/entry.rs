//! Tray entries and the hard-coded mapping from entity domains to services.
//!
//! Security note: the configuration never contains service names. The service
//! that runs when an entry is clicked is derived from the entity's domain
//! through [`action_service`]. A tampered configuration therefore cannot make
//! hassdash4linux call arbitrary Home Assistant services.

use serde::{Deserialize, Serialize};

/// Maximum length of a user-defined label (characters).
pub const MAX_LABEL_CHARS: usize = 100;
/// Maximum length of an entity ID (bytes, ASCII only).
pub const MAX_ENTITY_ID_LEN: usize = 255;

/// The kind of a tray entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Runs something when clicked (automation, scene, script, button).
    Action,
    /// Opens a control window when clicked (light, switch/outlet).
    Entity,
    /// Shows a value; cannot be clicked.
    Sensor,
    /// A separator line.
    Separator,
}

impl EntryKind {
    /// Domains allowed for this kind.
    pub fn allowed_domains(self) -> &'static [&'static str] {
        match self {
            EntryKind::Action => &["automation", "scene", "script", "button", "input_button"],
            EntryKind::Entity => &["light", "switch"],
            EntryKind::Sensor => &["sensor", "binary_sensor"],
            EntryKind::Separator => &[],
        }
    }
}

/// One entry of the tray menu, as stored in the configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entity_id: String,
    /// Optional label overriding the entity's friendly name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Automations only: skip the automation's conditions (like the "Run"
    /// button in Home Assistant). Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_condition: Option<bool>,
}

impl Entry {
    pub fn new(kind: EntryKind, entity_id: impl Into<String>) -> Self {
        Self {
            kind,
            entity_id: entity_id.into(),
            label: None,
            skip_condition: None,
        }
    }

    pub fn separator() -> Self {
        Self::new(EntryKind::Separator, "")
    }

    /// The entity's domain (the part before the dot).
    pub fn domain(&self) -> &str {
        domain_of(&self.entity_id)
    }

    /// Validate the entry. Returns a human-readable reason on failure.
    pub fn validate(&self) -> Result<(), String> {
        if self.kind == EntryKind::Separator {
            if !self.entity_id.is_empty() || self.label.is_some() || self.skip_condition.is_some() {
                return Err(
                    "a separator must not have an entity_id, label or skip_condition".into(),
                );
            }
            return Ok(());
        }
        if !is_valid_entity_id(&self.entity_id) {
            return Err(format!("invalid entity_id {:?}", self.entity_id));
        }
        if !self.kind.allowed_domains().contains(&self.domain()) {
            return Err(format!(
                "entity {:?} is not allowed for entries of kind {:?}",
                self.entity_id, self.kind
            ));
        }
        if let Some(label) = &self.label {
            if label.chars().count() > MAX_LABEL_CHARS {
                return Err(format!("label of {:?} is too long", self.entity_id));
            }
            if label.chars().any(char::is_control) {
                return Err(format!(
                    "label of {:?} contains control characters",
                    self.entity_id
                ));
            }
        }
        if self.skip_condition.is_some() && self.domain() != "automation" {
            return Err(format!(
                "skip_condition is only valid for automations ({:?})",
                self.entity_id
            ));
        }
        Ok(())
    }
}

/// The domain of an entity ID (`light` for `light.kitchen`).
pub fn domain_of(entity_id: &str) -> &str {
    entity_id.split_once('.').map(|(d, _)| d).unwrap_or("")
}

/// Whether `id` looks like a Home Assistant entity ID (`domain.object_id`,
/// lower-case ASCII letters, digits and underscores).
pub fn is_valid_entity_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ENTITY_ID_LEN {
        return false;
    }
    let Some((domain, object)) = id.split_once('.') else {
        return false;
    };
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    ok(domain) && ok(object)
}

/// A service call derived from an action entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceCall {
    pub domain: &'static str,
    pub service: &'static str,
    pub entity_id: String,
    pub data: serde_json::Map<String, serde_json::Value>,
}

/// The service that runs when an action entry is clicked.
///
/// This is the only place where hassdash4linux decides which Home Assistant
/// service an entry may call.
pub fn action_service(entry: &Entry) -> Option<ServiceCall> {
    if entry.kind != EntryKind::Action || entry.validate().is_err() {
        return None;
    }
    let mut data = serde_json::Map::new();
    let (domain, service) = match entry.domain() {
        "automation" => {
            data.insert(
                "skip_condition".into(),
                entry.skip_condition.unwrap_or(true).into(),
            );
            ("automation", "trigger")
        }
        "scene" => ("scene", "turn_on"),
        "script" => ("script", "turn_on"),
        "button" => ("button", "press"),
        "input_button" => ("input_button", "press"),
        _ => return None,
    };
    Some(ServiceCall {
        domain,
        service,
        entity_id: entry.entity_id.clone(),
        data,
    })
}

/// Services the entity window may call, per domain.
pub fn entity_service_allowed(domain: &str, service: &str) -> bool {
    matches!(
        (domain, service),
        ("light", "turn_on" | "turn_off" | "toggle")
            | ("switch", "turn_on" | "turn_off" | "toggle")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_ids() {
        assert!(is_valid_entity_id("light.kitchen_2"));
        assert!(!is_valid_entity_id("light"));
        assert!(!is_valid_entity_id("light."));
        assert!(!is_valid_entity_id("Light.kitchen"));
        assert!(!is_valid_entity_id("light.kit chen"));
        assert!(!is_valid_entity_id("light.kitchen.x"));
        assert!(!is_valid_entity_id("hassio.host_shutdown;"));
    }

    #[test]
    fn domains_per_kind() {
        assert!(
            Entry::new(EntryKind::Action, "scene.movie")
                .validate()
                .is_ok()
        );
        assert!(
            Entry::new(EntryKind::Action, "light.movie")
                .validate()
                .is_err()
        );
        assert!(
            Entry::new(EntryKind::Entity, "switch.coffee")
                .validate()
                .is_ok()
        );
        assert!(
            Entry::new(EntryKind::Sensor, "binary_sensor.door")
                .validate()
                .is_ok()
        );
        assert!(
            Entry::new(EntryKind::Sensor, "shell_command.x")
                .validate()
                .is_err()
        );
        assert!(Entry::separator().validate().is_ok());
        let mut e = Entry::new(EntryKind::Action, "scene.movie");
        e.skip_condition = Some(false);
        assert!(e.validate().is_err());
    }

    #[test]
    fn services() {
        let mut e = Entry::new(EntryKind::Action, "automation.good_night");
        let s = action_service(&e).unwrap();
        assert_eq!((s.domain, s.service), ("automation", "trigger"));
        assert_eq!(s.data["skip_condition"], serde_json::Value::Bool(true));
        e.skip_condition = Some(false);
        assert_eq!(
            action_service(&e).unwrap().data["skip_condition"],
            serde_json::Value::Bool(false)
        );
        let s = action_service(&Entry::new(EntryKind::Action, "input_button.bell")).unwrap();
        assert_eq!((s.domain, s.service), ("input_button", "press"));
        assert!(action_service(&Entry::new(EntryKind::Entity, "light.x")).is_none());
        assert!(entity_service_allowed("light", "turn_on"));
        assert!(!entity_service_allowed("lock", "unlock"));
    }
}
