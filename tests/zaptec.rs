//! Mapping tests on responses shaped like Zaptec's, and the plugin against a
//! fake Zaptec with HomewrightHQ played over a pipe.

mod host;

use homewright_zaptec::model;
use homewright_zaptec::Zaptec;
use serde_json::{json, Value};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const C1: &str = "c1a2b3c4-0000-4000-8000-000000000001";
const C2: &str = "c1a2b3c4-0000-4000-8000-000000000002";
const INST: &str = "a1a1a1a1-0000-4000-8000-000000000100";

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn chargers_from_the_list() {
    let c = model::parse_chargers(&fixture("chargers.json"));
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].id, C1);
    assert_eq!(c[0].name, "Carport");
    assert_eq!(c[0].serial.as_deref(), Some("ZAP012345"));
    assert_eq!(c[0].installation_id, INST);
    assert_eq!(c[0].online, Some(true));
    assert_eq!(c[1].online, Some(false));
}

#[test]
fn charging_observations() {
    let obs = model::observations(&fixture("state_charging.json"));
    // a null value is left out
    assert!(!obs.contains_key(&911));
    assert_eq!(model::status(&obs), Some("charging"));
    assert!(model::charging(&obs));
    assert_eq!(model::power(&obs), Some(11040.5));
    assert_eq!(model::session_energy(&obs), Some(7.412));
    assert_eq!(model::total_energy(&obs), Some(2345.678));
    assert_eq!(model::online(&obs, Some(false)), Some(true));
    let a = model::power_attributes(&obs);
    assert_eq!(a["current_phase1"], 15.9);
    assert_eq!(a["voltage_phase3"], 232.0);
    assert_eq!(a["charge_current_set"], 16.0);

    let idle = model::observations(&fixture("state_idle.json"));
    assert_eq!(model::status(&idle), Some("disconnected"));
    assert!(!model::charging(&idle));
    assert_eq!(model::total_energy(&idle), None);
    assert_eq!(model::online(&idle, Some(true)), Some(false));
    assert_eq!(model::online(&Default::default(), Some(true)), Some(true));

    let mut odd = idle.clone();
    odd.insert(model::OBS_OPERATION_MODE, "4".into());
    assert_eq!(model::status(&odd), Some("unknown"));
}

#[test]
fn meter_value_in_wh() {
    let mut obs = model::Obs::new();
    obs.insert(model::OBS_SIGNED_METER, r#"OCMF|{"RD":[{"RV":1500,"RI":"1-0:1.8.0","RU":"Wh"}]}|{"SD":"00"}"#.into());
    assert_eq!(model::total_energy(&obs), Some(1.5));
    obs.insert(model::OBS_SIGNED_METER, "not ocmf".into());
    assert_eq!(model::total_energy(&obs), None);
}

#[test]
fn installation_current() {
    let inst = fixture("installation.json");
    assert_eq!(model::available_current(&inst), Some(20.0));
    assert_eq!(model::max_current(&inst), 25.0);
    assert_eq!(model::max_current(&json!({})), model::DEFAULT_MAX_CURRENT);
    assert_eq!(model::available_current(&json!({ "AvailableCurrent": 10 })), Some(10.0));
    assert_eq!(model::clamp_current(30.4, 25.0), 25.0);
    assert_eq!(model::clamp_current(-3.0, 25.0), 0.0);
    assert_eq!(model::clamp_current(12.6, 25.0), 13.0);
    assert_eq!(model::current_body(13.0), json!({ "availableCurrent": 13.0 }));
}

async fn cloud(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("username=a%40b.dk"))
        .and(body_string_contains("password=secret"))
        .and(body_string_contains("grant_type=password"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("token.json")))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/chargers"))
        .and(header("Authorization", "Bearer TOKEN-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("chargers.json")))
        .mount(server)
        .await;
    for (id, file) in [(C1, "state_charging.json"), (C2, "state_idle.json")] {
        Mock::given(method("GET"))
            .and(path(format!("/api/chargers/{id}/state")))
            .and(header("Authorization", "Bearer TOKEN-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(file)))
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/api/installation/{INST}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("installation.json")))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/api/installation/{INST}/update")))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
    for cmd in [506, 507] {
        Mock::given(method("POST"))
            .and(path(format!("/api/chargers/{C1}/sendCommand/{cmd}")))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }
}

fn flow(data: Value) -> Value {
    json!({ "flow_id": "f1", "source": "user", "data": data })
}

#[tokio::test(flavor = "multi_thread")]
async fn flow_setup_and_commands() {
    let server = MockServer::start().await;
    cloud(&server).await;
    let h = host::FakeHost::start(Zaptec::default());
    let base = json!({ "base_url": server.uri() });

    let r = h.call("config_flow", json!({ "flow": flow(base.clone()), "step": "user", "input": null })).await.unwrap();
    assert_eq!(r["result"]["type"], "form");
    let keys: Vec<&String> = r["result"]["schema"]["properties"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["username", "password"]);

    let r = h.call("config_flow", json!({ "flow": flow(base.clone()), "step": "user", "input": { "username": "a@b.dk", "password": "wrong" } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "invalid_auth");

    let r = h.call("config_flow", json!({ "flow": flow(base.clone()), "step": "user", "input": { "username": " a@b.dk ", "password": "secret" } })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["title"], "Zaptec (a@b.dk)");
    assert_eq!(r["unique_id"], "a@b.dk");
    let data = r["result"]["data"].clone();

    h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "zaptec", "data": data } })).await.unwrap();
    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(rec.devices.len(), 2);
        assert_eq!(rec.devices[0]["serial_number"], "ZAP012345");
        assert_eq!(rec.services.len(), 1);
        assert_eq!(rec.services[0].0, "set_charging_current");
        assert_eq!(rec.services[0].1["fields"]["current"]["required"], true);
    }

    assert_eq!(h.state(&h.entity(&format!("{C1}-status"))).unwrap().0, "charging");
    let (power, attrs) = h.state(&h.entity(&format!("{C1}-power"))).unwrap();
    assert_eq!(power, "11040.5");
    assert_eq!(attrs["current_phase2"], 15.8);
    assert_eq!(h.state(&h.entity(&format!("{C1}-session_energy"))).unwrap().0, "7.412");
    assert_eq!(h.state(&h.entity(&format!("{C1}-total_energy"))).unwrap().0, "2345.678");
    assert_eq!(h.state(&h.entity(&format!("{C1}-online"))).unwrap().0, "on");
    let switch = h.entity(&format!("{C1}-charging"));
    assert_eq!(h.state(&switch).unwrap().0, "on");
    let number = h.entity(&format!("{C1}-current_limit"));
    let (limit, attrs) = h.state(&number).unwrap();
    assert_eq!(limit, "20");
    assert_eq!(attrs["max"], 25.0);
    let (spec, handler) = h.rec.lock().unwrap().entities[&number].clone();
    assert_eq!(spec["name"], "Dynamisk strømgrænse");
    assert_eq!(spec["capabilities"]["max"], 25.0);
    assert!(handler);

    // the idle charger has no meter value, so no total energy sensor
    assert_eq!(h.state(&h.entity(&format!("{C2}-status"))).unwrap().0, "disconnected");
    assert_eq!(h.state(&h.entity(&format!("{C2}-online"))).unwrap().0, "off");
    assert!(!h.rec.lock().unwrap().entities.values().any(|(s, _)| s["unique_id"] == format!("{C2}-total_energy")));

    let call = |entity: String, service: &str, data: Value| json!({ "entry_id": "e1", "entity_id": entity, "service": service, "data": data });
    h.call("entity/call_service", call(switch.clone(), "turn_off", json!({}))).await.unwrap();
    assert_eq!(h.state(&switch).unwrap().0, "off");
    assert_eq!(h.state(&h.entity(&format!("{C1}-status"))).unwrap().0, "connected_finished");
    h.call("entity/call_service", call(switch.clone(), "toggle", json!({}))).await.unwrap();
    assert_eq!(h.state(&switch).unwrap().0, "on");

    h.call("entity/call_service", call(number.clone(), "set_value", json!({ "value": "40" }))).await.unwrap();
    assert_eq!(h.state(&number).unwrap().0, "25");
    // the other charger in the installation shows the same limit
    assert_eq!(h.state(&h.entity(&format!("{C2}-current_limit"))).unwrap().0, "25");

    let svc = |data: Value, ids: Value| json!({ "entry_id": "e1", "service": "set_charging_current", "data": data, "entity_ids": ids, "return_response": false });
    h.call("service/call", svc(json!({ "current": 6 }), json!([switch.clone()]))).await.unwrap();
    assert_eq!(h.state(&number).unwrap().0, "6");
    // one installation, so no target is needed
    h.call("service/call", svc(json!({ "current": 10.4 }), json!([]))).await.unwrap();
    assert_eq!(h.state(&number).unwrap().0, "10");
    let err = h.call("service/call", svc(json!({}), json!([]))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "invalid_argument");
    let err = h.call("service/call", svc(json!({ "current": 6 }), json!(["light.stue"]))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "invalid_argument");

    let err = h.call("entity/call_service", call(number.clone(), "turn_on", json!({}))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "not_supported");

    let reqs = server.received_requests().await.unwrap();
    let commands: Vec<String> = reqs.iter().filter(|r| r.url.path().contains("/sendCommand/")).map(|r| r.url.path().rsplit('/').next().unwrap().to_string()).collect();
    assert_eq!(commands, ["506", "507"]);
    let updates: Vec<Value> = reqs.iter().filter(|r| r.url.path().ends_with("/update")).map(|r| serde_json::from_slice(&r.body).unwrap()).collect();
    assert_eq!(updates, [json!({ "availableCurrent": 25.0 }), json!({ "availableCurrent": 6.0 }), json!({ "availableCurrent": 10.0 })]);
    // one login in the flow and one for the whole setup
    assert_eq!(reqs.iter().filter(|r| r.url.path() == "/oauth/token" && String::from_utf8_lossy(&r.body).contains("secret")).count(), 2);

    let diag = h.call("diagnostics", json!({ "entry": { "entry_id": "e1", "data": { "username": "a@b.dk", "password": "secret" } } })).await.unwrap();
    assert_eq!(diag["entry"]["password"], "**REDACTED**");
    assert_eq!(diag["chargers"].as_array().unwrap().len(), 2);

    let opts = h.call("options_flow", json!({ "flow": { "flow_id": "o1", "source": "options", "entry": { "entry_id": "e1", "data": {} } }, "step": "init", "input": { "scan_interval": 5 } })).await.unwrap();
    assert_eq!(opts["result"]["data"]["scan_interval"], 30);
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_token_logs_in_again_and_rate_limit_is_not_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("token.json")))
        .mount(&server)
        .await;
    // the first answer is 401, the next one works
    Mock::given(method("GET"))
        .and(path("/api/chargers"))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/chargers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "Pages": 1, "Data": [] })))
        .mount(&server)
        .await;
    let c = homewright_zaptec::api::Client::new(&server.uri(), "a@b.dk", "x");
    assert_eq!(c.chargers().await.unwrap()["Data"], json!([]));
    let logins = server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == "/oauth/token").count();
    assert_eq!(logins, 2);

    let limited = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "600"))
        .mount(&limited)
        .await;
    let c = homewright_zaptec::api::Client::new(&limited.uri(), "a@b.dk", "x");
    assert_eq!(c.login().await.unwrap_err(), homewright_zaptec::api::ApiError::RateLimited(Some(std::time::Duration::from_secs(600))));
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_fails_setup_as_auth() {
    let server = MockServer::start().await;
    cloud(&server).await;
    let h = host::FakeHost::start(Zaptec::default());
    let err = h
        .call("setup_entry", json!({ "entry": { "entry_id": "e2", "domain": "zaptec", "data": { "base_url": server.uri(), "username": "a@b.dk", "password": "old" } } }))
        .await
        .unwrap_err();
    assert_eq!(err["data"]["kind"], "auth_failed");
}
