//! Human-readable entity states for the tray menu and windows.
//!
//! Everything returned here is plain text; callers must never interpret it
//! as markup.

use hd_core::i18n::{language, tr};

use crate::EntityState;

/// Format a number with `decimals` places using the UI language's decimal separator.
pub fn number(value: f64, decimals: Option<usize>) -> String {
    let s = match decimals {
        Some(d) => format!("{value:.d$}", d = d.min(6)),
        None => {
            let s = format!("{value}");
            if s.contains('e') {
                format!("{value:.3}")
            } else {
                s
            }
        }
    };
    if language() == Some("de") {
        s.replace('.', ",")
    } else {
        s
    }
}

/// `value unit`, with no space before `%` (English) as Home Assistant does.
fn with_unit(value: &str, unit: Option<&str>) -> String {
    match unit.map(str::trim).filter(|u| !u.is_empty()) {
        None => value.to_owned(),
        Some("%") if language() != Some("de") => format!("{value}%"),
        Some(u) => format!("{value} {}", hd_core::truncate_chars(u, 20)),
    }
}

/// The value of a sensor or binary sensor. `display_precision` comes from the
/// entity registry, if known.
pub fn sensor_value(st: &EntityState, display_precision: Option<usize>) -> String {
    if st.is_unavailable() {
        return "—".to_owned();
    }
    if st.domain() == "binary_sensor" {
        return binary_sensor_value(st).to_owned();
    }
    let unit = st.attr_str("unit_of_measurement");
    let value = match st.state.parse::<f64>() {
        Ok(f) if f.is_finite() => number(f, display_precision),
        _ => hd_core::truncate_chars(&st.state, 60),
    };
    with_unit(&value, unit)
}

/// On/off wording for binary sensors, depending on the device class.
pub fn binary_sensor_value(st: &EntityState) -> &'static str {
    let on = st.is_on();
    let pick = |a: &'static str, b: &'static str| if on { a } else { b };
    match st.attr_str("device_class").unwrap_or("") {
        "door" | "garage_door" | "opening" | "window" => pick(tr("Open"), tr("Closed")),
        "motion" | "occupancy" | "sound" | "vibration" | "moving" | "gas" | "smoke"
        | "carbon_monoxide" | "tamper" | "safety" | "problem" => pick(tr("Detected"), tr("Clear")),
        "presence" => pick(tr("Home"), tr("Away")),
        "moisture" => pick(tr("Wet"), tr("Dry")),
        "lock" => pick(tr("Unlocked"), tr("Locked")),
        "battery" => pick(tr("Low"), tr("Normal")),
        "connectivity" => pick(tr("Connected"), tr("Disconnected")),
        "plug" => pick(tr("Plugged in"), tr("Unplugged")),
        "running" => pick(tr("Running"), tr("Not running")),
        _ => pick(tr("On"), tr("Off")),
    }
}

/// Brightness in percent (0–100) of a light.
pub fn brightness_percent(st: &EntityState) -> Option<u8> {
    st.attr_f64("brightness")
        .map(|b| ((b / 255.0) * 100.0).round().clamp(0.0, 100.0) as u8)
}

/// Short state of a light or switch, e.g. `On · 60 %`.
pub fn entity_state(st: &EntityState) -> String {
    if st.is_unavailable() {
        return tr("Unavailable").to_owned();
    }
    match (st.state.as_str(), st.domain()) {
        ("on", "light") => match brightness_percent(st) {
            Some(p) => format!("{} · {}", tr("On"), with_unit(&p.to_string(), Some("%"))),
            None => tr("On").to_owned(),
        },
        ("on", _) => tr("On").to_owned(),
        ("off", _) => tr("Off").to_owned(),
        (other, _) => hd_core::truncate_chars(other, 40),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn st(id: &str, state: &str, attrs: serde_json::Value) -> EntityState {
        EntityState {
            entity_id: id.into(),
            state: state.into(),
            attributes: attrs.as_object().cloned().unwrap(),
            last_changed: None,
        }
    }

    #[test]
    fn sensors() {
        // The test environment's language is not forced; only check structure.
        let s = st("sensor.t", "21.456", json!({"unit_of_measurement": "°C"}));
        let v = sensor_value(&s, Some(1));
        assert!(v == "21.5 °C" || v == "21,5 °C", "{v}");
        assert_eq!(
            sensor_value(&st("sensor.t", "unavailable", json!({})), None),
            "—"
        );
        let b = st("binary_sensor.d", "on", json!({"device_class": "door"}));
        assert_eq!(binary_sensor_value(&b), tr("Open"));
        assert_eq!(
            sensor_value(&st("sensor.mode", "eco", json!({})), None),
            "eco"
        );
    }

    #[test]
    fn lights() {
        let l = st("light.k", "on", json!({"brightness": 153}));
        assert_eq!(brightness_percent(&l), Some(60));
        assert!(entity_state(&l).starts_with(tr("On")));
        assert_eq!(entity_state(&st("switch.s", "off", json!({}))), tr("Off"));
    }
}
