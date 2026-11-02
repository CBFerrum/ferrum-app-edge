<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/banner-dark.svg">
  <img alt="Ferrum IoT Edge" src="docs/assets/banner-light.svg" width="100%">
</picture>

[![CI](https://github.com/CBFerrum/ferrum-app-edge/actions/workflows/ci.yml/badge.svg)](https://github.com/CBFerrum/ferrum-app-edge/actions/workflows/ci.yml) ![Version](https://img.shields.io/badge/version-0.3.1-0369a1?style=flat-square) ![Ferrum](https://img.shields.io/badge/Ferrum-%3E%3D0.3%2C%20%3C0.4-2f5bea?style=flat-square) ![License](https://img.shields.io/badge/license-AGPL--3.0-3b82f6?style=flat-square) ![Rust](https://img.shields.io/badge/Rust-native%20app-b7410e?style=flat-square&logo=rust&logoColor=white) ![Interface](https://img.shields.io/badge/interface-React%20%2B%20Vite-0ea5e9?style=flat-square&logo=react&logoColor=white) ![MCP](https://img.shields.io/badge/AI%20agents-MCP-8b5cf6?style=flat-square)

**A native app for [Ferrum](https://github.com/CBFerrum/Ferrum), the open-source, AI-first business platform.**

[Overview](#overview) · [Highlights](#highlights) · [Screenshots](#screenshots) · [How it works](#how-it-works) · [The gateway](#the-gateway) · [Data model](#data-model) · [Install](#install) · [Develop](#develop) · [Changelog](#changelog)

</div>

---

## Overview

Ferrum IoT Edge connects equipment to Ferrum without writing a driver. A device is described in
a few lines of YAML: how to reach it, how to read it and how to command it. Those definitions run on
`ferrum-site`, the gateway in this repository, which reads, decides and keeps working on location
through outages and power cuts, and catches up when the link returns. Devices can also call it
themselves, with ThingsBoard's own topics and paths, so their firmware needs no change.

Logic is drawn on a canvas, simulated, reviewed by a second person, and then runs at a gateway or on
the platform, and you can watch real messages pass through it. Dashboards, alarms that escalate,
firmware rollouts and exports to the usual buses and clouds are all here. The strongest thing a flow
can do is ask for a command, and Ferrum's ledger decides whether it goes out.

Ferrum coordinates equipment and does not control it. Motion, machine sequencing and every safety
function stay with their own controllers.


| At a glance | |
|---|---|
| 📦 Version | `0.3.1` |
| 🧱 Platform | Ferrum `>=0.3, <0.4` |
| 🔗 Requires | ops `>=0.1` |
| 🖥️ Interface | its own, at `/edge/` |
| 🗃️ Records | 18 entities in 1 package |
| 👥 Roles | 3: Edge Viewer, Edge Engineer, Edge Reviewer |
| 📊 Reports | 0 |
| 🧪 Tests | 359 Rust tests, 6 interface test files |


> [!IMPORTANT]
> **Nothing here has run on physical equipment.** Every protocol was tried against simulators, stand-ins and other people's libraries, and no vendor's device, robot or PLC has been connected. Nothing is certified, and nothing here is a safety claim. [Compatibility](docs/compatibility.md) records what was tried on what, and what was not.


## Highlights

<table>
<tr>
<td width="50%" valign="top">

### 📝 Devices as data
One line per thing to read, for S7, EtherNet/IP, ADS, BACnet, OPC UA, Modbus, SNMP, KNX, IEC 104, DNP3, DLMS and M-Bus, and a few lines for TCP, serial, HTTP, MQTT, Kafka, AMQP, Sparkplug B and MTConnect. Anything else is an external driver in any language.

</td>
<td width="50%" valign="top">

### 📡 Devices that call in
MQTT, HTTP and CoAP on ThingsBoard's own topics and paths, so its own client library works unchanged, in CI. LwM2M, OCPP 1.6 and 2.0.1 for EV chargers, and DTLS for what leaves a private network, with tokens, passwords or certificates.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🧩 49 templates
VDA 5050, Open-RMF, openTCS, ROS 2 through rosbridge, ChirpStack, The Things Stack, Sigfox, Zigbee2MQTT and more, and whole solutions in one file.

</td>
<td width="50%" valign="top">

### 🔀 Flows
A canvas of 44 kinds of step, with no code and no loops, simulated by the code a gateway runs, approved by someone else, and watched live. At the gateway offline, or on the platform across sites.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🛑 Asks, never acts
A flow, a schedule or a rollout asks the ledger for a command. An approved envelope or a person decides, so nothing here moves equipment by itself.

</td>
<td width="50%" valign="top">

### 📴 Works offline
Reading, deciding, alarms and the commands a gateway already holds carry on through outages, crashes and restores, from a journal on disk. New work comes from the platform.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 📊 Dashboards and alarms
Drag-and-drop dashboards with shared links and live updates, history charts, alarms that are assigned, shelved and escalated, and reports by the clock.

</td>
<td width="50%" valign="top">

### 🤝 Customers of your own
They sign in to your portal and see their devices, their alarms and the dashboards you gave them, and nothing else of the workspace.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🚚 A fleet at once
Import from CSV, tags and assets, bulk commands, schedules, and firmware rollouts that stop by themselves. Never for a robot, a controller or anything a safety function runs on.

</td>
<td width="50%" valign="top">

### 🔗 Everywhere else
NATS, Kafka, RabbitMQ, InfluxDB, ClickHouse, PostgreSQL, S3, SQS, SNS, Pub/Sub, Azure and Sparkplug, held until delivered. A gateway as an OPC UA server, read only, plus peers, relays, twins, Thing Descriptions, Asset Administration Shells and EPCIS.

</td>
</tr>
</table>

## Screenshots

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/edge-home-dark.webp">
  <img alt="The overview: gateways, what they reach, how the fleet stands and what needs a person." src="docs/assets/edge-home-light.webp" width="100%">
</picture>

<p align="center"><sub>The overview: gateways, what they reach, how the fleet stands and what needs a person.</sub></p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/edge-board-dark.webp">
  <img alt="A dashboard: gauges, charts, alarms and controls, laid out by hand, with commands going through the ledger." src="docs/assets/edge-board-light.webp" width="100%">
</picture>

<p align="center"><sub>A dashboard: gauges, charts, alarms and controls, laid out by hand, with commands going through the ledger.</sub></p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/edge-flow-dark.webp">
  <img alt="A flow: logic drawn as steps and wires, simulated and reviewed, and watched live while it runs at the gateway." src="docs/assets/edge-flow-light.webp" width="100%">
</picture>

<p align="center"><sub>A flow: logic drawn as steps and wires, simulated and reviewed, and watched live while it runs at the gateway.</sub></p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/edge-devices-dark.webp">
  <img alt="Devices: found by tag and site, imported from a file, and asked for something all at once." src="docs/assets/edge-devices-light.webp" width="100%">
</picture>

<p align="center"><sub>Devices: found by tag and site, imported from a file, and asked for something all at once.</sub></p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/edge-robots-dark.webp">
  <img alt="Robots: where each one says it is, on a plan of the site." src="docs/assets/edge-robots-light.webp" width="100%">
</picture>

<p align="center"><sub>Robots: where each one says it is, on a plan of the site.</sub></p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/edge-definition-dark.webp">
  <img alt="A definition: a device in a few lines, tried on a sample with nothing attached." src="docs/assets/edge-definition-light.webp" width="100%">
</picture>

<p align="center"><sub>A definition: a device in a few lines, tried on a sample with nothing attached.</sub></p>

> [!TIP]
> The screenshots follow your GitHub theme: switch between light and dark to see both.

## How it works

### From equipment to Ferrum

```mermaid
flowchart LR
    subgraph Site["On location"]
        EQ["🏭 Equipment<br/><small>robots · PLCs · sensors</small>"]
        BOX["A box in a cabinet<br/><small>a peer, over a signed link</small>"]
        GW["ferrum-site<br/><small>definitions · flows · journal</small>"]
    end
    subgraph Platform["Ferrum"]
        APP["IoT Edge<br/><small>definitions · flows · alarms</small>"]
        OPS["Operations<br/><small>command ledger · identity · telemetry</small>"]
    end
    OUT["Historian · broker · cloud"]
    EQ <--> GW
    BOX <--> GW
    GW -- "signed, when it can" --> OPS
    APP -- "what each gateway should be" --> GW
    APP --> OPS
    GW -- "held until delivered" --> OUT
    classDef app fill:#0369a1,color:#fff,stroke:#0369a1
    class APP,GW app
```

### Inside Ferrum

```mermaid
flowchart LR
    subgraph Browser
        UI["Its interface<br/><small>/edge/</small>"]
        BAR["Ferrum bar<br/><small>launcher · search · bell</small>"]
    end
    subgraph Server["Ferrum server"]
        API["REST · MCP · live"]
        K["Kernel<br/><small>permissions · row-level security · audit</small>"]
        APP["IoT Edge<br/><small>18 entities · 3 roles</small>"]
        OB[("Outbox")]
    end
    DB[("PostgreSQL")]
    UI --> API --> K --> DB
    BAR --> API
    K <--> APP
    K --> OB --> EV["Notifications · webhooks<br/>automations"]
    APP -. requires .-> D0["ops >=0.1"]
    classDef app fill:#0369a1,color:#fff,stroke:#0369a1
    class APP app
```

Everything IoT Edge adds is ordinary Ferrum metadata and Rust: its records get lists, forms,
search, permissions, an audit trail, exports and the REST and MCP APIs for free, so AI agents work
with them under the same rules as people.

### What it adds to Ferrum

| Where | What |
|---|---|
| 🚀 Launcher | **IoT Edge** at `/edge/` |
| ➕ Create menu | Device definition |
| ➕ Create menu | Flow |
| ➕ Create menu | Dashboard |
| 🔔 Notification | Alarms raised and cleared |
| 🌐 Public page | Dashboard |
| 🧑‍💼 Portal | Your devices |

## The gateway

`ferrum-site` is one binary with a SQLite journal, and no broker of its own. Equipment that needs one
gets Mosquitto beside it, so that a fault in one leaves the other standing.

```bash
cargo build --release --locked -p ferrum-site
```

| Guide | |
|---|---|
| [Running a gateway](docs/gateway.md) | Installing, enrolling, backing up, updating, boxes in cabinets, closed networks, exports |
| [Definitions](docs/definitions.md) | Describing a device |
| [Flows](docs/flows.md) | Drawing logic that runs at the gateway |
| [Looking after a fleet](docs/fleet.md) | Importing, asking many at once, firmware, schedules, solutions |
| [Parity](docs/parity.md) | What ThingsBoard and the rest do, capability by capability, and where this stands |
| [Compatibility](docs/compatibility.md) | What was tried on what, and what was not |
| [Licenses](docs/licenses.md), [accepted advisories](docs/advisories.yaml) | What a gateway compiles in, and what was reviewed |

`deploy/` holds the systemd unit, a starting `site.yaml` and a Mosquitto configuration.

## Data model

The main records and how they connect (18 entities in all; `packages/` has every field).

```mermaid
erDiagram
    DeviceDefinition {
        string definition_name PK
        string status
        string body
    }
    EdgeRule {
        string rule_name PK
        string metric
        string comparison
    }
    EdgeAlarm {
        string summary PK
        string status
    }
    EdgeAsset {
        string asset_name PK
    }
    EdgeFirmware {
        string firmware_name PK
        string firmware_version
        string url
    }
    EdgeFlow {
        string flow_name PK
        string status
    }
    DeviceDefinition ||--o{ EdgeRule : "definition"
    EdgeRule ||--o{ EdgeAlarm : "rule"
    EdgeFlow ||--o{ EdgeAlarm : "flow"
    DeviceDefinition ||--o{ EdgeFirmware : "definition"
```

### Roles

| Role | What it is for | Entities |
|---|---|---|
| **Edge Viewer** | Sees gateways, devices, alarms and robots. | 17 |
| **Edge Engineer** | Writes and publishes definitions, rules and exports, and acknowledges alarms. | 20 |
| **Edge Reviewer** | Reviews flows drawn by others, and decides when they run. | 8 |

## Install

Workspace administrators install it from **Apps** (`/store/`) in Ferrum; it ships in the
official Ferrum server image. It needs **ops** `>=0.1` installed first. Self-hosters building their own image enable the
`edge` feature of the server. Everything it adds is also available to AI agents over
Ferrum's MCP server, within each person's permissions.

## Develop

Clone it next to the platform:

```text
work/
├── ferrum/            https://github.com/CBFerrum/Ferrum (with Ferrum-Frontend cloned into ferrum/web)
└── ferrum-app-edge/
```

Point the platform dependencies at your checkout (this file is git-ignored):

```toml
# .cargo/config.toml
[patch."https://github.com/CBFerrum/Ferrum"]
ferrum-calc = { path = "../ferrum/crates/calc" }
ferrum-domain = { path = "../ferrum/crates/domain" }
ferrum-kernel = { path = "../ferrum/crates/kernel" }
ferrum-meta = { path = "../ferrum/crates/meta" }
ferrum-ops = { path = "../ferrum/crates/ops" }
ferrum-ops-contract = { path = "../ferrum/crates/ops-contract" }
```

The tests need PostgreSQL with an ordinary (non-superuser) role, so row-level security
applies:

```bash
createuser ferrum --pwprompt && createdb -O ferrum ferrum_test
cargo test
```


The interface lives in `ui/` and builds with Ferrum-Frontend's packages:

```bash
cd ui
npm run build      # ui/dist, checked against the size budget
npm test
```

With a development server started with `FERRUM_DEV_UI=1`, `ferrum-app dev --ui` serves the pages
from Vite behind the real bar and session, with hot reload.


The gateway's recovery test kills the real binary at named points, which takes a second database and
a build that can crash on purpose. `scripts/mutants.py` then breaks each guarantee in turn, to check
that a test notices.

```bash
createdb -O ferrum ferrum_site_test
cargo test -p ferrum-site --features failpoints --test recovery_it
```


### Project layout

```text
app.yaml      the manifest: id, version, requirements, what it adds to Ferrum
packages/     metadata: entities, roles, reports, workflows, print formats
src/          the app's logic in Rust: hooks, actions, endpoints, upkeep jobs
ui/           its own interface (React + Vite), built into ui/dist
tests/        integration tests against a real PostgreSQL
contract/     ferrum-edge-contract: definitions, flows and Sparkplug, pure code a simulation and a gateway share
gateway/      ferrum-site, the gateway: journal, transports, flows, links between servers, a console
templates/    the twenty definitions to start from
deploy/       the systemd unit, a starting configuration, Mosquitto beside the gateway
docs/         guides, what was tried on what, licenses and accepted advisories
scripts/      mutants, the license inventory, the advisory check
CHANGELOG.md  what changed in each version
```

## Changelog

| Version | Highlights |
|---|---|
| **0.3.1** | The gateway's releases are static, so they run on any 64-bit Linux. Those of 0.3.0 needed glibc 2.39 and would not start on Ubuntu 22.04, Debian … |
| **0.3.0** | Customers who own devices: their people see those devices, their alarms and the dashboards given to them in the workspace's portal, and nothing else. |
| **0.2.0** | Devices that call in: MQTT 3.1 to 5, HTTP and CoAP on ThingsBoard's own topics and paths, with tokens, passwords and certificates, provisioning … |
| **0.1.0** | Definitions: devices described in a few lines of YAML over TCP, UDP, serial, HTTP, WebSocket, MQTT, Modbus TCP and RTU, OPC UA, listeners and … |

Every version is in [CHANGELOG.md](CHANGELOG.md).

## License

[AGPL-3.0-or-later](LICENSE), like the platform. Part of [Ferrum](https://github.com/CBFerrum/Ferrum) ·
[Report an issue](https://github.com/CBFerrum/ferrum-app-edge/issues)
