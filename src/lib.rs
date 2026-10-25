//! Zaptec charging stations through Zaptec's cloud API, as an external
//! integration for HomewrightHQ.
//!
//! The user logs in with the e-mail and password from the Zaptec portal or
//! app. Each charger becomes a device with status, power, session energy,
//! the meter reading, online, a switch that resumes or pauses charging and a
//! number for the current limit. Zaptec limits current per installation, so
//! the number and the `zaptec.set_charging_current` service change the
//! installation's available current, shared by all its chargers.

pub mod api;
pub mod model;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use homewright_plugin::{async_trait, json, Device, Entity, Entry, Error, Flow, FlowReply, Host, Integration, Map, Value};

use api::{ApiError, Client};
use model::{ChargerInfo, Obs};

pub const DOMAIN: &str = "zaptec";
/// Default and allowed range for reading each charger, in seconds.
pub const DEFAULT_SCAN_SECONDS: u64 = 60;
const MIN_SCAN_SECONDS: u64 = 30;
const MAX_SCAN_SECONDS: u64 = 600;
/// Zaptec asks for the charger list at most once an hour.
const LIST_POLL: Duration = Duration::from_secs(3600);
/// How often the installations (the current limit) are read.
const INSTALLATION_POLL: Duration = Duration::from_secs(600);
/// After a command the charger may still report the old mode for a while.
const SETTLE: Duration = Duration::from_secs(20);
/// The pause after a 429 without Retry-After, and the shortest pause.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(5 * 60);

/// Logging to HomewrightHQ's log through stderr.
#[doc(hidden)]
pub fn emit(level: &str, message: String) {
    homewright_plugin::log(level, message);
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::emit("INFO", format!($($t)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::emit("WARN", format!($($t)*)) };
}

fn login_schema(username: &str) -> Value {
    json!({
        "type": "object",
        "required": ["username", "password"],
        "properties": {
            "username": { "type": "string", "title": "E-mail", "default": username, "description": "Den e-mail, du logger ind med på Zaptec-portalen eller i appen" },
            "password": { "type": "string", "format": "password", "title": "Adgangskode" }
        }
    })
}

fn options_schema(seconds: u64) -> Value {
    json!({
        "type": "object",
        "properties": {
            "scan_interval": {
                "type": "integer", "title": "Sekunder mellem opdateringer", "default": seconds,
                "minimum": MIN_SCAN_SECONDS, "maximum": MAX_SCAN_SECONDS,
                "description": "Zaptec beder om, at man ikke spørger for tit. Hver ladeboks hentes for sig."
            }
        }
    })
}

fn service_description() -> Value {
    json!({
        "name": "Sæt ladestrøm",
        "description": "Sætter den strøm, installationen må bruge til opladning. Zaptec styrer strømmen pr. installation, så det gælder alle ladebokse i installationen. 0 A stopper opladningen.",
        "target": { "entity": { "domain": "number" } },
        "fields": {
            "current": {
                "name": "Strøm",
                "description": "Ampere pr. fase",
                "required": true,
                "example": 16,
                "selector": { "number": { "min": 0, "max": 32, "step": 1, "unit": "A" } }
            }
        }
    })
}

fn base_url(get: impl Fn(&str) -> Option<String>) -> String {
    get("base_url").unwrap_or_else(|| api::BASE_URL.to_string())
}

fn error_code(e: &ApiError) -> &'static str {
    match e {
        ApiError::Auth => "invalid_auth",
        ApiError::RateLimited(_) => "rate_limited",
        ApiError::Other(_) => "cannot_connect",
    }
}

pub fn scan_interval(entry: &Entry) -> Duration {
    let s = entry
        .options
        .get("scan_interval")
        .or_else(|| entry.get("scan_interval"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_SCAN_SECONDS)
        .clamp(MIN_SCAN_SECONDS, MAX_SCAN_SECONDS);
    Duration::from_secs(s)
}

#[derive(Default)]
pub struct Zaptec {
    accounts: Mutex<HashMap<String, Arc<Account>>>,
}

impl Zaptec {
    fn account(&self, entry_id: &str) -> Result<Arc<Account>, Error> {
        self.accounts.lock().unwrap().get(entry_id).cloned().ok_or_else(|| Error::unavailable("opsætningen kører ikke"))
    }
}

#[async_trait]
impl Integration for Zaptec {
    async fn config_flow(&self, _host: &Host, flow: Flow, step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        match (step, input) {
            ("user", None) => Ok(FlowReply::form("user", login_schema(""))),
            ("reauth", None) => {
                let user = flow.entry.as_ref().and_then(|e| e.get_str("username")).unwrap_or_default().to_string();
                Ok(FlowReply::form("user", login_schema(&user)))
            }
            ("user", Some(v)) => {
                let username = v["username"].as_str().unwrap_or_default().trim().to_string();
                let password = v["password"].as_str().unwrap_or_default().to_string();
                let base = base_url(|k| flow.data.get(k).and_then(Value::as_str).map(String::from));
                let client = Client::new(&base, &username, &password);
                let chargers = match client.chargers().await {
                    Ok(list) => model::parse_chargers(&list),
                    Err(e) => {
                        info!("{e}");
                        return Ok(FlowReply::form_with_errors("user", login_schema(&username), json!({ "base": error_code(&e) })));
                    }
                };
                if flow.source == "reauth" {
                    let mut data = flow.entry.as_ref().and_then(|e| e.data.as_object().cloned()).unwrap_or_default();
                    data.insert("username".into(), json!(username));
                    data.insert("password".into(), json!(password));
                    return Ok(FlowReply::create("", Value::Object(data)));
                }
                if chargers.is_empty() {
                    return Ok(FlowReply::abort("no_devices_found"));
                }
                let mut data = flow.data.clone();
                data.insert("username".into(), json!(username));
                data.insert("password".into(), json!(password));
                Ok(FlowReply::create(&format!("Zaptec ({username})"), Value::Object(data)).with_unique_id(username.to_lowercase()))
            }
            (s, _) => Err(Error::invalid(format!("ukendt trin {s}"))),
        }
    }

    async fn options_flow(&self, _host: &Host, flow: Flow, _step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let current = flow.entry.as_ref().map(|e| scan_interval(e).as_secs()).unwrap_or(DEFAULT_SCAN_SECONDS);
        match input {
            None => Ok(FlowReply::form("init", options_schema(current))),
            Some(v) => {
                let s = v["scan_interval"].as_u64().unwrap_or(current).clamp(MIN_SCAN_SECONDS, MAX_SCAN_SECONDS);
                Ok(FlowReply::create("", json!({ "scan_interval": s })))
            }
        }
    }

    async fn setup_entry(&self, host: &Host, entry: Entry) -> Result<(), Error> {
        let entry_id = entry.entry_id.clone();
        let username = entry.get_str("username").ok_or_else(|| Error::auth_failed("e-mail mangler"))?.to_string();
        let password = entry.get_str("password").unwrap_or_default().to_string();
        let client = Arc::new(Client::new(&base_url(|k| entry.get_str(k).map(String::from)), &username, &password));

        let chargers = match client.chargers().await {
            Ok(list) => model::parse_chargers(&list),
            Err(ApiError::Auth) => return Err(Error::auth_failed("Zaptec afviste e-mail eller adgangskode")),
            Err(e) => return Err(Error::not_ready(e)),
        };
        let _ = host.delete_issue(&entry_id, "auth").await;

        let account = Arc::new(Account {
            entry_id: entry_id.clone(),
            host: host.clone(),
            client: client.clone(),
            chargers: Mutex::new(HashMap::new()),
            installations: Mutex::new(HashMap::new()),
            entities: Mutex::new(vec![]),
            task: Mutex::new(None),
            paused_until: Mutex::new(None),
        });

        let mut installation_ids: Vec<String> = chargers.iter().map(|c| c.installation_id.clone()).filter(|i| !i.is_empty()).collect();
        installation_ids.sort();
        installation_ids.dedup();
        for id in installation_ids {
            match client.installation(&id).await {
                Ok(v) => {
                    account.installations.lock().unwrap().insert(id, v);
                }
                Err(e) => warn!("kunne ikke læse installationen {id}: {e}"),
            }
        }

        for info in chargers {
            let obs = match client.charger_state(&info.id).await {
                Ok(v) => model::observations(&v),
                Err(e) => return Err(Error::not_ready(e)),
            };
            account.register(&info, &obs).await?;
            let id = info.id.clone();
            account.chargers.lock().unwrap().insert(id.clone(), Charger { available: true, info, obs, last_control: None });
            account.publish(&id).await;
        }
        host.register_service(&entry_id, "set_charging_current", Some(service_description())).await?;

        let interval = scan_interval(&entry);
        let acc = account.clone();
        let task = tokio::spawn(async move {
            let (mut last_list, mut last_inst) = (Instant::now(), Instant::now());
            loop {
                tokio::time::sleep(interval).await;
                let list_due = last_list.elapsed() >= LIST_POLL;
                let inst_due = last_inst.elapsed() >= INSTALLATION_POLL;
                acc.poll(list_due, inst_due).await;
                if list_due {
                    last_list = Instant::now();
                }
                if inst_due {
                    last_inst = Instant::now();
                }
            }
        });
        *account.task.lock().unwrap() = Some(task);
        if let Some(old) = self.accounts.lock().unwrap().insert(entry_id, account) {
            old.stop();
        }
        Ok(())
    }

    async fn unload_entry(&self, _host: &Host, entry_id: &str) -> Result<(), Error> {
        if let Some(a) = self.accounts.lock().unwrap().remove(entry_id) {
            a.stop();
        }
        Ok(())
    }

    async fn entity_service(&self, _host: &Host, entry_id: &str, entity_id: &str, service: &str, data: Map<String, Value>) -> Result<(), Error> {
        self.account(entry_id)?.call_service(entity_id, service, &data).await
    }

    async fn domain_service(&self, _host: &Host, entry_id: &str, service: &str, data: Map<String, Value>, entity_ids: Vec<String>) -> Result<Value, Error> {
        if service != "set_charging_current" {
            return Err(Error::not_supported(service));
        }
        let account = self.account(entry_id)?;
        let amps = model::number_arg(&data, "current").ok_or_else(|| Error::invalid("current mangler"))?;
        let mut targets = entity_ids;
        match data.get("entity_id") {
            Some(Value::String(s)) => targets.push(s.clone()),
            Some(Value::Array(a)) => targets.extend(a.iter().filter_map(Value::as_str).map(String::from)),
            _ => {}
        }
        for inst in account.installations_for(&targets)? {
            account.set_current(&inst, amps).await?;
        }
        Ok(Value::Null)
    }

    async fn diagnostics(&self, _host: &Host, entry: Entry) -> Value {
        let mut d = entry.data.clone();
        if let Value::Object(m) = &mut d {
            m.insert("password".into(), json!("**REDACTED**"));
        }
        let (chargers, installations) = self
            .accounts
            .lock()
            .unwrap()
            .get(&entry.entry_id)
            .map(|a| {
                let chargers: Vec<Value> = a
                    .chargers
                    .lock()
                    .unwrap()
                    .values()
                    .map(|c| {
                        let obs: Map<String, Value> = c.obs.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
                        json!({ "name": c.info.name, "installation": c.info.installation_id, "available": c.available, "observations": obs })
                    })
                    .collect();
                let inst: Vec<Value> = a.installations.lock().unwrap().values().cloned().collect();
                (chargers, inst)
            })
            .unwrap_or_default();
        json!({ "entry": d, "chargers": chargers, "installations": installations })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Status,
    Power,
    SessionEnergy,
    TotalEnergy,
    Online,
    Charging,
    CurrentLimit,
}

type StateUpdate = (String, Map<String, Value>);

struct Charger {
    info: ChargerInfo,
    obs: Obs,
    available: bool,
    last_control: Option<Instant>,
}

struct Account {
    entry_id: String,
    host: Host,
    client: Arc<Client>,
    chargers: Mutex<HashMap<String, Charger>>,
    /// installation id -> `/api/installation/{id}`
    installations: Mutex<HashMap<String, Value>>,
    /// (entity_id, charger id, kind)
    entities: Mutex<Vec<(String, String, Kind)>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    paused_until: Mutex<Option<Instant>>,
}

fn num_state(v: Option<f64>) -> Option<StateUpdate> {
    v.map(|t| (t.to_string(), Map::new()))
}

fn number_attributes(max: f64) -> Map<String, Value> {
    let mut a = Map::new();
    a.insert("min".into(), json!(0.0));
    a.insert("max".into(), json!(max));
    a.insert("step".into(), json!(1.0));
    a.insert("mode".into(), json!("box"));
    a
}

impl Account {
    fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
    }

    fn installation(&self, id: &str) -> Value {
        self.installations.lock().unwrap().get(id).cloned().unwrap_or(Value::Null)
    }

    async fn register(&self, info: &ChargerInfo, obs: &Obs) -> Result<(), Error> {
        let host = &self.host;
        let dev = host
            .register_device(
                &self.entry_id,
                Device {
                    identifiers: vec![(DOMAIN.into(), info.id.clone())],
                    name: info.name.clone(),
                    manufacturer: Some("Zaptec".into()),
                    serial_number: info.serial.clone(),
                    configuration_url: Some("https://portal.zaptec.com".into()),
                    ..Default::default()
                },
            )
            .await?;
        let id = &info.id;
        let entity = |key: &str, name: &str, domain: &str| Entity::new(domain, format!("{id}-{key}")).name(name).device(&dev);
        let max = model::max_current(&self.installation(&info.installation_id));
        let mut list: Vec<(Kind, Entity, bool)> = vec![
            (
                Kind::Status,
                entity("status", "Status", "sensor").device_class("enum").icon("mdi:ev-station").capabilities(json!({ "options": model::status_options() })),
                false,
            ),
            (Kind::Power, entity("power", "Effekt", "sensor").device_class("power").unit("W").state_class("measurement"), false),
            (Kind::SessionEnergy, entity("session_energy", "Energi denne opladning", "sensor").device_class("energy").unit("kWh").state_class("total"), false),
        ];
        if model::total_energy(obs).is_some() {
            list.push((Kind::TotalEnergy, entity("total_energy", "Energi i alt", "sensor").device_class("energy").unit("kWh").state_class("total_increasing"), false));
        }
        list.push((Kind::Online, entity("online", "Online", "binary_sensor").device_class("connectivity").category("diagnostic"), false));
        list.push((Kind::Charging, entity("charging", "Opladning", "switch").icon("mdi:ev-plug-type2"), true));
        if !info.installation_id.is_empty() {
            list.push((
                Kind::CurrentLimit,
                entity("current_limit", "Dynamisk strømgrænse", "number")
                    .device_class("current")
                    .unit("A")
                    .icon("mdi:current-ac")
                    .capabilities(json!({ "min": 0.0, "max": max, "step": 1.0, "mode": "box" })),
                true,
            ));
        }
        for (kind, e, handler) in list {
            if let Some(entity_id) = host.add_entity(&self.entry_id, e, handler).await? {
                self.entities.lock().unwrap().push((entity_id, id.clone(), kind));
            }
        }
        Ok(())
    }

    /// Push states for every entity of a charger.
    async fn publish(&self, charger_id: &str) {
        let (available, updates): (bool, Vec<(String, Option<StateUpdate>)>) = {
            let chargers = self.chargers.lock().unwrap();
            let Some(c) = chargers.get(charger_id) else { return };
            let inst = self.installation(&c.info.installation_id);
            let online = model::online(&c.obs, c.info.online);
            let list = self
                .entities
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, d, _)| d == charger_id)
                .map(|(entity_id, _, kind)| {
                    let value = match kind {
                        Kind::Status => model::status(&c.obs).map(|s| (s.to_string(), Map::new())),
                        Kind::Power => model::power(&c.obs).map(|p| (p.to_string(), model::power_attributes(&c.obs))),
                        Kind::SessionEnergy => num_state(model::session_energy(&c.obs)),
                        Kind::TotalEnergy => model::total_energy(&c.obs).map(|e| (format!("{e:.3}"), Map::new())),
                        Kind::Online => online.map(|o| (if o { "on" } else { "off" }.to_string(), Map::new())),
                        Kind::Charging => Some((if model::charging(&c.obs) { "on" } else { "off" }.to_string(), Map::new())),
                        Kind::CurrentLimit => model::available_current(&inst).map(|a| (a.to_string(), number_attributes(model::max_current(&inst)))),
                    };
                    (entity_id.clone(), value)
                })
                .collect();
            (c.available, list)
        };
        for (entity_id, value) in updates {
            let res = match value {
                _ if !available => self.host.set_unavailable(&entity_id).await,
                Some((state, attrs)) => self.host.set_state(&entity_id, state, attrs).await,
                None => self.host.set_state(&entity_id, "unknown", Map::new()).await,
            };
            if let Err(e) = res {
                warn!("kunne ikke opdatere {entity_id}: {e}");
            }
        }
    }

    fn paused(&self) -> bool {
        self.paused_until.lock().unwrap().is_some_and(|t| Instant::now() < t)
    }

    async fn poll(&self, list_due: bool, installations_due: bool) {
        if self.paused() {
            return;
        }
        if list_due {
            match self.client.chargers().await {
                Ok(list) => {
                    let mut chargers = self.chargers.lock().unwrap();
                    for info in model::parse_chargers(&list) {
                        if let Some(c) = chargers.get_mut(&info.id) {
                            c.info.online = info.online;
                        }
                    }
                }
                Err(e) => {
                    self.report(&e).await;
                    if self.paused() {
                        return;
                    }
                }
            }
        }
        if installations_due {
            let ids: Vec<String> = self.installations.lock().unwrap().keys().cloned().collect();
            for id in ids {
                match self.client.installation(&id).await {
                    Ok(v) => {
                        self.installations.lock().unwrap().insert(id, v);
                    }
                    Err(e) => {
                        self.report(&e).await;
                        if self.paused() {
                            return;
                        }
                    }
                }
            }
        }
        let ids: Vec<String> = self.chargers.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let settling = self.chargers.lock().unwrap().get(&id).and_then(|c| c.last_control).is_some_and(|t| t.elapsed() < SETTLE);
            if !settling {
                match self.client.charger_state(&id).await {
                    Ok(v) => {
                        let _ = self.host.delete_issue(&self.entry_id, "auth").await;
                        if let Some(c) = self.chargers.lock().unwrap().get_mut(&id) {
                            c.obs = model::observations(&v);
                            c.available = true;
                        }
                    }
                    Err(e) => {
                        self.report(&e).await;
                        if let ApiError::RateLimited(_) = e {
                            return;
                        }
                        if let Some(c) = self.chargers.lock().unwrap().get_mut(&id) {
                            c.available = false;
                        }
                    }
                }
            }
            self.publish(&id).await;
        }
    }

    async fn report(&self, e: &ApiError) {
        match e {
            ApiError::Auth => {
                let r = self
                    .host
                    .create_issue(
                        &self.entry_id,
                        "auth",
                        "error",
                        "Zaptec: login afvist",
                        "Zaptec afviste e-mail eller adgangskode. Godkend integrationen igen med de rigtige oplysninger.",
                    )
                    .await;
                if let Err(e) = r {
                    warn!("kunne ikke oprette problem: {e}");
                }
            }
            ApiError::RateLimited(after) => {
                let pause = after.unwrap_or(RATE_LIMIT_PAUSE).max(RATE_LIMIT_PAUSE);
                warn!("Zaptec svarede 429; venter {} minutter", pause.as_secs() / 60);
                *self.paused_until.lock().unwrap() = Some(Instant::now() + pause);
            }
            ApiError::Other(err) => warn!("{err}"),
        }
    }

    /// The installations behind some of this account's entities. With no
    /// entities, the only installation, if there is just one.
    fn installations_for(&self, entity_ids: &[String]) -> Result<Vec<String>, Error> {
        let chargers = self.chargers.lock().unwrap();
        let mut out: Vec<String> = if entity_ids.is_empty() {
            chargers.values().map(|c| c.info.installation_id.clone()).collect()
        } else {
            let entities = self.entities.lock().unwrap();
            entity_ids
                .iter()
                .map(|e| {
                    let (_, charger, _) = entities.iter().find(|(id, _, _)| id == e).ok_or_else(|| Error::invalid(format!("{e} hører ikke til Zaptec")))?;
                    Ok(chargers.get(charger).map(|c| c.info.installation_id.clone()).unwrap_or_default())
                })
                .collect::<Result<_, Error>>()?
        };
        out.retain(|i| !i.is_empty());
        out.sort();
        out.dedup();
        if out.is_empty() {
            return Err(Error::invalid("ingen installation at sætte strømmen på"));
        }
        if entity_ids.is_empty() && out.len() > 1 {
            return Err(Error::invalid("vælg en ladeboks; kontoen har flere installationer"));
        }
        Ok(out)
    }

    /// Set an installation's available current and update its chargers.
    async fn set_current(&self, installation_id: &str, amps: f64) -> Result<(), Error> {
        if self.paused() {
            return Err(Error::unavailable("Zaptec har bedt om en pause efter for mange forespørgsler"));
        }
        let max = model::max_current(&self.installation(installation_id));
        let amps = model::clamp_current(amps, max);
        if let Err(e) = self.client.update_installation(installation_id, &model::current_body(amps)).await {
            self.report(&e).await;
            return Err(Error::failed(e));
        }
        {
            let mut inst = self.installations.lock().unwrap();
            let v = inst.entry(installation_id.to_string()).or_insert_with(|| json!({}));
            if let Value::Object(m) = v {
                m.insert("AvailableCurrent".into(), json!(amps));
                for p in 1..=3 {
                    m.insert(format!("AvailableCurrentPhase{p}"), json!(amps));
                }
            }
        }
        let ids: Vec<String> = self.chargers.lock().unwrap().values().filter(|c| c.info.installation_id == installation_id).map(|c| c.info.id.clone()).collect();
        for id in ids {
            self.publish(&id).await;
        }
        Ok(())
    }

    async fn call_service(&self, entity_id: &str, service: &str, data: &Map<String, Value>) -> Result<(), Error> {
        let (charger_id, kind) = self
            .entities
            .lock()
            .unwrap()
            .iter()
            .find(|(e, _, _)| e == entity_id)
            .map(|(_, c, k)| (c.clone(), *k))
            .ok_or_else(|| Error::new("not_found", entity_id))?;
        let (charging, installation_id) = {
            let chargers = self.chargers.lock().unwrap();
            let c = chargers.get(&charger_id).ok_or_else(|| Error::unavailable(entity_id))?;
            (model::charging(&c.obs), c.info.installation_id.clone())
        };
        match (kind, service) {
            (Kind::Charging, "turn_on" | "turn_off" | "toggle") => {
                let on = match service {
                    "turn_on" => true,
                    "turn_off" => false,
                    _ => !charging,
                };
                if self.paused() {
                    return Err(Error::unavailable("Zaptec har bedt om en pause efter for mange forespørgsler"));
                }
                let cmd = if on { api::CMD_RESUME_CHARGING } else { api::CMD_STOP_CHARGING_FINAL };
                if let Err(e) = self.client.send_command(&charger_id, cmd).await {
                    self.report(&e).await;
                    return Err(Error::failed(e));
                }
                if let Some(c) = self.chargers.lock().unwrap().get_mut(&charger_id) {
                    // show the expected mode until the charger reports it
                    c.obs.insert(model::OBS_OPERATION_MODE, if on { "3" } else { "5" }.into());
                    c.last_control = Some(Instant::now());
                }
                self.publish(&charger_id).await;
                Ok(())
            }
            (Kind::CurrentLimit, "set_value") => {
                let v = model::number_arg(data, "value").ok_or_else(|| Error::invalid("value mangler"))?;
                self.set_current(&installation_id, v).await
            }
            _ => Err(Error::not_supported(service)),
        }
    }
}
