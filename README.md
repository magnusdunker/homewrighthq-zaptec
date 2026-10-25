# Zaptec for HomewrightHQ

An external integration for [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for Zaptec chargers (Go, Go 2 and Pro) through Zaptec's cloud API.

## Installation

In HomewrightHQ: **Settings → Integrations → From GitHub**. Paste

```
https://github.com/magnusdunker/homewrighthq-zaptec
```

and press **Install**. Then add **Zaptec** under **Add integration**, and log in with the email and password you use on the Zaptec portal or in the app.

## What you get

Each charger becomes a device with:

- status: `disconnected`, `connected_requesting`, `charging`, `connected_finished` or `unknown`
- power in W, with current and voltage per phase and the allocated charging current as attributes
- energy in the ongoing charging session (kWh)
- total energy (kWh) from the charger's meter reading, where the charger sends one
- online (connection to Zaptec)
- the switch **Opladning** (charging): on resumes charging, off pauses it
- the number **Dynamisk strømgrænse** (dynamic current limit) in amperes, from 0 to the installation's maximum

The integration also registers the service `zaptec.set_charging_current` with the field `current` (amperes). It is for automations that charge in the cheap hours: for example, set 16 A when the price is low and 0 A when it is high.

```yaml
action: zaptec.set_charging_current
target:
  entity_id: number.carport_dynamisk_stromgraense
data:
  current: 16
```

If the account has only one installation, `target` can be left out.

## The current limit applies to the whole installation

Zaptec controls the current per installation and not per charger. The number and the service set the installation's available current (`availableCurrent`), so all chargers in the same installation get the same limit, and the change shows on all of them. The value is rounded to whole amperes and kept between 0 and the installation's `MaxCurrent` (32 A if Zaptec does not report it).

Zaptec recommends changing the available current at most every 15 minutes. The integration does not limit this itself, so let automations change it rarely.

## Updates and Zaptec's limits

The integration reads each charger every minute, the installations every ten minutes and the list of chargers once an hour, as Zaptec asks. Under **Options** on the integration, the time between updates can be set from 30 to 600 seconds. If Zaptec answers 429 (too many requests), the integration pauses for at least five minutes, or as long as Zaptec asks.

After the switch is pressed, the integration shows the expected state right away and waits 20 seconds before it reads the charger again.

## Not tested

The integration is tested against a fake Zaptec with responses shaped like the real ones, not against real chargers.

Checked in Zaptec's documentation (docs.zaptec.com):

- login with `POST /oauth/token`, `grant_type=password`, and the response fields `access_token` and `expires_in`
- the observations 710 (`ChargerOperationMode` 1, 2, 3, 5), 513 (power in W), 553 (energy in the charging session in kWh), 554 (`SignedMeterValue`), -2 (online), 501-503 (voltage), 507-509 (current) and 708 (allocated current)
- the commands 506 (`StopChargingFinal`, pause) and 507 (`ResumeCharging`)
- that installation lists should be fetched at most once an hour, and the available current changed at most every 15 minutes

Not checked against the real service:

- the path `POST /api/chargers/{id}/sendCommand/{command}` and that it accepts an empty body
- `POST /api/installation/{id}/update` with `{ "availableCurrent": A }`, and that it sets all three phases
- the fields `MaxCurrent`, `AvailableCurrent` and `AvailableCurrentPhase1`-`3` in `GET /api/installation/{id}`
- the fields in `GET /api/chargers` (`Data`, `Id`, `DeviceId`, `Name`, `InstallationId`, `IsOnline`), and that the list is not split over several pages for an ordinary home
- the format of `GET /api/chargers/{id}/state` (`StateId`, `ValueAsString`)
- that the meter reading in observation 554 is an OCMF string with the reading in `RD[].RV` and the unit in `RU`; if it is not there, the total energy sensor is not shown
- that 400 from `/oauth/token` means a wrong password
- how a Go 2 and a Pro differ from a Go in the observations

Zaptec's Service Bus (live updates without polling) and 3-to-1 phase switching are not included.

## Development

Built in Rust on the `thehome-plugin` SDK from [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: login and Zaptec's API
- `src/model.rs`: Zaptec's data and how it becomes states and commands
- `src/lib.rs`: the connection to HomewrightHQ
- `tests/zaptec.rs`: tests against a fake Zaptec with the responses in `tests/fixtures`, and a fake HomewrightHQ in `tests/host`

```
cargo test
cargo build --release
```

A new version is released with a tag, e.g. `git tag v0.1.0 && git push origin v0.1.0`. The workflow builds for Windows, Linux (x86_64 and arm64, static musl) and macOS and puts the binaries and `homewright.json` on the release.

## License

MIT
