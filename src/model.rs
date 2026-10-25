//! Zaptec's data and how it becomes states and commands.
//!
//! A charger's live values are observations: a list of `StateId` and
//! `ValueAsString`. The ids come from Zaptec's state observation reference.
//! The current limit belongs to the installation, not the charger, so every
//! charger in an installation shares it.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

pub const OBS_ONLINE: i64 = -2;
pub const OBS_VOLTAGE: [i64; 3] = [501, 502, 503];
pub const OBS_CURRENT: [i64; 3] = [507, 508, 509];
pub const OBS_POWER: i64 = 513;
pub const OBS_SESSION_ENERGY: i64 = 553;
pub const OBS_SIGNED_METER: i64 = 554;
pub const OBS_CHARGE_CURRENT_SET: i64 = 708;
pub const OBS_OPERATION_MODE: i64 = 710;

/// `ChargerOperationMode` (observation 710) and the state it becomes.
pub const STATUS: [(i64, &str); 4] = [(1, "disconnected"), (2, "connected_requesting"), (3, "charging"), (5, "connected_finished")];

/// The default when the installation does not say how much it can take.
pub const DEFAULT_MAX_CURRENT: f64 = 32.0;

/// One charger from `/api/chargers`.
#[derive(Clone, Debug, PartialEq)]
pub struct ChargerInfo {
    pub id: String,
    /// The serial number, such as `ZAP000123`.
    pub serial: Option<String>,
    pub name: String,
    pub installation_id: String,
    pub online: Option<bool>,
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
}

/// The chargers in `{ "Data": [...] }`, or in a bare list.
pub fn parse_chargers(v: &Value) -> Vec<ChargerInfo> {
    let list = v.get("Data").and_then(Value::as_array).or_else(|| v.as_array());
    list.into_iter()
        .flatten()
        .filter_map(|c| {
            let id = str_field(c, "Id")?;
            let serial = str_field(c, "DeviceId");
            let name = str_field(c, "Name").or_else(|| serial.clone()).unwrap_or_else(|| "Zaptec".into());
            Some(ChargerInfo {
                installation_id: str_field(c, "InstallationId").unwrap_or_default(),
                online: c.get("IsOnline").and_then(Value::as_bool),
                id,
                serial,
                name,
            })
        })
        .collect()
}

/// Observations by id from `/api/chargers/{id}/state`.
pub fn observations(v: &Value) -> HashMap<i64, String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let id = o.get("StateId").and_then(Value::as_i64)?;
            let value = match o.get("ValueAsString") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => return None,
                Some(other) => other.to_string(),
            };
            Some((id, value))
        })
        .collect()
}

pub type Obs = HashMap<i64, String>;

pub fn num(obs: &Obs, id: i64) -> Option<f64> {
    obs.get(&id).and_then(|s| s.trim().parse::<f64>().ok()).filter(|v| v.is_finite())
}

pub fn status(obs: &Obs) -> Option<&'static str> {
    let mode = num(obs, OBS_OPERATION_MODE)? as i64;
    Some(STATUS.iter().find(|(m, _)| *m == mode).map(|(_, s)| *s).unwrap_or("unknown"))
}

pub fn status_options() -> Vec<&'static str> {
    let mut v: Vec<&str> = STATUS.iter().map(|(_, s)| *s).collect();
    v.push("unknown");
    v
}

pub fn charging(obs: &Obs) -> bool {
    num(obs, OBS_OPERATION_MODE) == Some(3.0)
}

/// Online from observation -2, else from the charger list.
pub fn online(obs: &Obs, listed: Option<bool>) -> Option<bool> {
    num(obs, OBS_ONLINE).map(|v| v != 0.0).or(listed)
}

/// Power in W.
pub fn power(obs: &Obs) -> Option<f64> {
    num(obs, OBS_POWER)
}

/// Energy in the current session, kWh.
pub fn session_energy(obs: &Obs) -> Option<f64> {
    num(obs, OBS_SESSION_ENERGY)
}

/// The meter reading in kWh from the signed meter value, which is an OCMF
/// string: `OCMF|{ ... "RD": [ { "RV": 1234.5, "RU": "kWh" } ] }|{signature}`.
pub fn total_energy(obs: &Obs) -> Option<f64> {
    let raw = obs.get(&OBS_SIGNED_METER)?;
    let payload = raw.split('|').nth(1)?;
    let v: Value = serde_json::from_str(payload).ok()?;
    let readings = v.get("RD")?.as_array()?;
    let r = readings.iter().rev().find(|r| r.get("RI").and_then(Value::as_str).is_none_or(|ri| ri.contains("1.8.0")))?;
    let value = r.get("RV")?.as_f64()?;
    match r.get("RU").and_then(Value::as_str) {
        Some(u) if u.eq_ignore_ascii_case("Wh") => Some(value / 1000.0),
        _ => Some(value),
    }
}

/// Attributes for the power sensor: current and voltage per phase and the
/// current the charger has been given.
pub fn power_attributes(obs: &Obs) -> Map<String, Value> {
    let mut a = Map::new();
    for (i, id) in OBS_CURRENT.iter().enumerate() {
        if let Some(v) = num(obs, *id) {
            a.insert(format!("current_phase{}", i + 1), json!(v));
        }
    }
    for (i, id) in OBS_VOLTAGE.iter().enumerate() {
        if let Some(v) = num(obs, *id) {
            a.insert(format!("voltage_phase{}", i + 1), json!(v));
        }
    }
    if let Some(v) = num(obs, OBS_CHARGE_CURRENT_SET) {
        a.insert("charge_current_set".into(), json!(v));
    }
    a
}

fn installation_num(inst: &Value, key: &str) -> Option<f64> {
    inst.get(key).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

/// The installation's available current in A: `AvailableCurrent`, else the
/// lowest of the three phases.
pub fn available_current(inst: &Value) -> Option<f64> {
    installation_num(inst, "AvailableCurrent").or_else(|| {
        ["AvailableCurrentPhase1", "AvailableCurrentPhase2", "AvailableCurrentPhase3"]
            .iter()
            .filter_map(|k| installation_num(inst, k))
            .reduce(f64::min)
    })
}

/// The highest current the installation's fuse allows, in A.
pub fn max_current(inst: &Value) -> f64 {
    installation_num(inst, "MaxCurrent").filter(|m| *m > 0.0).unwrap_or(DEFAULT_MAX_CURRENT)
}

/// Round to whole amperes and keep inside 0..max.
pub fn clamp_current(a: f64, max: f64) -> f64 {
    a.round().clamp(0.0, max)
}

/// The body for `/api/installation/{id}/update`.
pub fn current_body(a: f64) -> Value {
    json!({ "availableCurrent": a })
}

/// The value of a number service call, as a number or a string.
pub fn number_arg(data: &Map<String, Value>, key: &str) -> Option<f64> {
    data.get(key).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).filter(|v| v.is_finite())
}
