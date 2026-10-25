# Zaptec til HomewrightHQ

En ekstern integration til [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for Zaptec-ladebokse (Go, Go 2 og Pro) gennem Zaptecs cloud-API.

## Installation

I HomewrightHQ: **Indstillinger → Integrationer → Fra GitHub**. Indsæt

```
https://github.com/magnusdunker/homewrighthq-zaptec
```

og tryk **Installér**. Tilføj derefter **Zaptec** under **Tilføj integration**, og log ind med den e-mail og adgangskode, du bruger på Zaptec-portalen eller i appen.

## Hvad du får

Hver ladeboks bliver en enhed med:

- status: `disconnected`, `connected_requesting`, `charging`, `connected_finished` eller `unknown`
- effekt i W, med strøm og spænding pr. fase og den tildelte ladestrøm som attributter
- energi i den igangværende opladning (kWh)
- energi i alt (kWh) fra boksens målerstand, hvor boksen sender en
- online (forbindelse til Zaptec)
- kontakten **Opladning**: tænd genoptager opladningen, sluk sætter den på pause
- tallet **Dynamisk strømgrænse** i ampere, fra 0 til installationens maksimum

Integrationen registrerer også tjenesten `zaptec.set_charging_current` med feltet `current` (ampere). Den er til automationer, der lader i de billige timer: sæt f.eks. 16 A, når prisen er lav, og 0 A, når den er høj.

```yaml
action: zaptec.set_charging_current
target:
  entity_id: number.carport_dynamisk_stromgraense
data:
  current: 16
```

Har kontoen kun én installation, kan `target` udelades.

## Strømgrænsen gælder hele installationen

Zaptec styrer strømmen pr. installation og ikke pr. ladeboks. Tallet og tjenesten sætter installationens tilgængelige strøm (`availableCurrent`), så alle ladebokse i samme installation får den samme grænse, og ændringen vises på dem alle. Værdien rundes til hele ampere og holdes mellem 0 og installationens `MaxCurrent` (32 A, hvis Zaptec ikke oplyser den).

Zaptec anbefaler at ændre den tilgængelige strøm højst hvert 15. minut. Integrationen begrænser det ikke selv, så lad automationer skifte sjældent.

## Opdatering og Zaptecs grænser

Integrationen læser hver ladeboks hvert minut, installationerne hvert tiende minut og listen over ladebokse en gang i timen, som Zaptec beder om. Under **Indstillinger** på integrationen kan tiden mellem opdateringer sættes fra 30 til 600 sekunder. Svarer Zaptec 429 (for mange forespørgsler), holder integrationen pause i mindst fem minutter, eller så længe Zaptec beder om.

Efter et tryk på kontakten viser integrationen den forventede tilstand med det samme og venter 20 sekunder, før den læser boksen igen.

## Ikke afprøvet

Integrationen er afprøvet mod en falsk Zaptec med svar formet som de rigtige, ikke mod rigtige ladebokse.

Kontrolleret i Zaptecs dokumentation (docs.zaptec.com):

- login med `POST /oauth/token`, `grant_type=password`, og svarfelterne `access_token` og `expires_in`
- observationerne 710 (`ChargerOperationMode` 1, 2, 3, 5), 513 (effekt i W), 553 (energi i opladningen i kWh), 554 (`SignedMeterValue`), -2 (online), 501-503 (spænding), 507-509 (strøm) og 708 (tildelt strøm)
- kommandoerne 506 (`StopChargingFinal`, pause) og 507 (`ResumeCharging`)
- at installationslister højst bør hentes en gang i timen, og den tilgængelige strøm højst ændres hvert 15. minut

Ikke kontrolleret mod den rigtige tjeneste:

- stien `POST /api/chargers/{id}/sendCommand/{kommando}` og at den tager imod et tomt body
- `POST /api/installation/{id}/update` med `{ "availableCurrent": A }`, og at det sætter alle tre faser
- felterne `MaxCurrent`, `AvailableCurrent` og `AvailableCurrentPhase1`-`3` i `GET /api/installation/{id}`
- felterne i `GET /api/chargers` (`Data`, `Id`, `DeviceId`, `Name`, `InstallationId`, `IsOnline`), og at listen ikke er delt over flere sider for et almindeligt hjem
- formatet af `GET /api/chargers/{id}/state` (`StateId`, `ValueAsString`)
- at målerstanden i observation 554 er en OCMF-streng med aflæsningen i `RD[].RV` og enheden i `RU`; findes den ikke, vises sensoren for energi i alt ikke
- at 400 fra `/oauth/token` betyder forkert adgangskode
- hvordan en Go 2 og en Pro adskiller sig fra en Go i observationerne

Zaptecs Service Bus (live opdateringer uden at spørge) og 3-til-1-faseskift er ikke med.

## Udvikling

Bygget i Rust på SDK'en `thehome-plugin` fra [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: login og Zaptecs API
- `src/model.rs`: Zaptecs data og hvordan de bliver til tilstande og kommandoer
- `src/lib.rs`: forbindelsen til HomewrightHQ
- `tests/zaptec.rs`: tests mod en falsk Zaptec med svarene i `tests/fixtures`, og en falsk HomewrightHQ i `tests/host`

```
cargo test
cargo build --release
```

En ny version udgives med et tag, f.eks. `git tag v0.1.0 && git push origin v0.1.0`. Workflowet bygger til Windows, Linux (x86_64 og arm64, statisk musl) og macOS og lægger binærerne og `homewright.json` på releasen.

## Licens

MIT
