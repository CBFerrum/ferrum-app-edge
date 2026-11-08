# Changelog

## 0.6.1

*Add what is known* takes `fresh`: what a device saw longer ago than that, or at no known time, is left out, so a site that lost its link does not take what it last heard for what is so now (Ferrum's Logic Platform, FLC-EDGE-008). Tests now show that no step moves equipment or reads a clock (a formula's only functions are exact arithmetic, the server's own), and that a site short of disk keeps no plain sample and takes waiting work up once it has room (FLC-EDGE-011, -012).

## 0.6.0

Business-critical flows (Ferrum's Logic Platform, W5; docs/flows.md). A flow marked `profile: critical` hears each reading with when the device saw it and how far it can be trusted (bad ones too, marked), leaves a reading it has had alone, and is written at its gateway with what it did after each step, in one transaction, so a power cut neither loses nor repeats a decision (tested by killing the gateway at either side of that write). New steps: *bring to units* (exact scale and offset, the raw value kept, what cannot be a reading quarantined), *by quality*, *average over the time seen* (event-time windows with a watermark, corrections within a declared lateness on `revised`, later readings on `late`, quiet devices closing what they have), *an episode* (a deadband, on and off delays, reopening, staleness that is not recovery, an identity that is the same however often it is said) and *open a case*. The platform keeps one Edge Condition for each episode, each reading once, and publishes `edge.condition_opened`, `_reopened`, `_closed` and `_interrupted` once each, so a process the workspace runs starts once per episode. Whoever runs a new revision chooses whether it starts empty, drains the running one first, or keeps what unchanged steps remembered. A flow keeps track of 50,000 things at most. Rebuilt with Ferrum-Frontend e60e032.

## 0.5.9

The reports it defines in code (their titles and parameters) have names in Arabic and Hindi too, with its models' and manifest's (281 each in ui/src/locales/names). Translated by the model, to be reviewed by native speakers.

## 0.5.8

Rebuilt with Ferrum-Frontend c59c9a2: the names of its models, fields and manifest in Arabic and Hindi (275 each in ui/src/locales/names, which the platform serves to every page of the workspace, so a record's labels, the launcher and the bar read in the person's language), and the sentences its pages still showed in English (split around a link or a figure, built in code from a template or word by word) whole and translated (593 words each in ui/src/locales). Translated by the model, to be reviewed by native speakers.

## 0.5.7

Rebuilt with Ferrum-Frontend 155518c: the sentences its pages built in code (counts, dates, what a toast or a screen reader says) go through t() too, as whole sentences where code chose a word, and carry built-in Arabic and Hindi (512 words each in ui/src/locales, translated by the model and to be reviewed by native speakers).

## 0.5.6

The assistant knows what its records mean in its pages (contributes.shell.assistant): their states, what counts and where figures come from, with questions it suggests asking there.

## 0.5.5

Rebuilt with Ferrum-Frontend 5b22e7b: its own words carry built-in Arabic and Hindi (496 words each, ui/src/locales, translated by the model and to be reviewed by native speakers), fetched for the page's language only. The labels it keeps in objects, its dialogs, its page titles and the words it keeps in variables go through t() as well, and the names and sizes it shows as they are keep their order right to left. Its pages are drawn once their words are in, and its entry carries only the parts of the kit it uses.

## 0.5.4

Rebuilt with Ferrum-Frontend 6537d65: an action's link, code or token shown once is kept in a dialog to copy, and on a page that reads right to left, English left untranslated keeps its own order. Its own words go through the kit's t() (364 places), so a workspace's own words for its language reach them, and Admin, Interface words lists them under it (dist/words.json).

## 0.5.3

Rebuilt with Ferrum-Frontend d5d98f2: a workspace's own words for its interface (Admin, Interface words) reach its pages as they load; an attachment field that holds a picture shows it; and a value another record already has is said so, where the form said the record had been changed by someone else. Its own screens' margins, paddings, borders, alignment and insets go by the line's start and end, so they mirror right to left as the kit's do; what it places by coordinates (boards, timelines, charts, handles) stays as it was.

## 0.5.2

Rebuilt with Ferrum-Frontend 63280d3: right to left, its pages mirror as they read (icons that point along the line, margins and alignment by the line's start and end, arrow keys along it), and the records pages' words are in Arabic and Hindi.

## 0.5.1

Rebuilt with Ferrum-Frontend a64416a: a new record's box whose default is 1 shows ticked, as it is saved.

## 0.5.0

Gateways in the support report (FERRUM-IND-021). The workspace's support report (`apps.edge`,
Ferrum PR 90) counts its gateways in service that have spoken, those not heard from for five
minutes, and those whose last word named problems, with those problems in words. Counts only:
no gateway's name or address, so a partner reading a customer's health with a health-only key
sees how the gateways are and nothing else. The control plane counts them in the customer's
health (Control Plane 0.5.0). Rebuilt with Ferrum-Frontend 922fcfb: an action's refusals on another
record's rows show in its dialog, and an action with a confirmation and parameters asks once.

## 0.4.7

Rebuilt with Ferrum-Frontend a76295e: its pages show times on a person's own clock when they chose one on their Account page, while today and due dates stay on the workspace's day.

## 0.4.6

A chart's axis and tip and the messages a flow is watched with are on the workspace's clock; the customer's portal keeps the customer's own. Rebuilt with Ferrum-Frontend 24c3951, so its records' pages show times on the workspace's clock too, a draft numbered on submit is headed as a draft, and a record of an app that is not installed says so.

## 0.4.5

Rebuilt with Ferrum-Frontend bb60115: a record page reads an empty number or check as the server does, so an action whose condition needs one shows, and a workspace's settings open as their one record.

## 0.4.4

Personal-data requests find the customers' people and the report recipients by their address, in the rows that hold them: a request copies those rows alone, and erasure removes them (the person loses the customer's portal, or stops getting the report) and leaves the customer and the schedule (Ferrum's FERRUM-PRIVACY-002).

## 0.4.3

Rebuilt with Ferrum-Frontend 00dea07: in a workflow state that names what may change, the other fields show read-only, and the fields the state needs show required.

## 0.4.2

Rebuilt with Ferrum-Frontend daeadda: after an action runs, a record page shows what the action said it did rather than only that it is done.

## 0.4.1

Rebuilt with Ferrum-Frontend 80ff179: a field's label names its control for assistive technology, and a record whose title is a link shows that record's title, not its id.

## Unreleased

**No charger or meter has been connected to any of this.**

- **What a charger says about money is kept before it hears that it was heard.** Starts, stops and meter
  readings reach the journal first, so a crash loses nothing a charger was told was safe. One that cannot be
  kept gets an error, and says it again. Saying the same thing twice, on any connection, counts once.
- **Transaction numbers survive restarts.** They came from a counter in memory, which started again and could
  hand a number a charger still held to someone else. The same start gets the same number.
- **Charging sessions.** A session is worked out from everything said about it, in whatever order it arrived.
  It is charging only once a register has moved, because a charger accepting a start has only said it will
  try. A reset, late, missing, unconverted or unverified reading gives no figure to bill on, and only a person
  can settle such a session, with a reason that stays on it.
- **Reconnecting is paced.** Charger and MQTT ports let in 100 new connections a second unless `rate=`
  says otherwise, so after an outage those already in are not held up behind everyone else's handshake.
  A charger's handshake that stalls is closed after ten seconds, where before it held its place forever.
- **A gateway says who is connected to it,** for the platform to give that gateway the device's work. A
  domain shared by several gateways still needs its devices given to each: that waits for the platform change.

## 0.4.0

From a review of the connected system under retries, crashes, configuration changes, clock changes
and physical exceptions. **Nothing here has met physical equipment.**

- **Recovery asks the device a command went to, and no other.** What a command went out on is written
  with its intent. Looking for it after a crash, following it and stopping it go back to exactly that,
  or nowhere: a device that merely takes the same operation is never asked, because its "never heard
  of it" would look like leave to send the command again. If what reaches a device has changed since,
  the outcome is unknown, for a person. **Finish running work before upgrading:** work a journal holds
  from before this has nothing to go back to, and becomes unknown.
- **Stopping one vehicle's order stops one vehicle.** `cancelOrder` went to every vehicle on an adapter,
  and it stops whatever a vehicle is doing. It now goes to the vehicle that has the order, once that
  vehicle says so. Over MQTT a cancel goes to the device's own topic, not to `all`.
- **A failure never says what an address held.** An HTTP failure quoted its URL, which can hold a
  `{secret.x}`, and a failure's reason goes to the platform. Secrets stay on the site again.
- **Time is the platform's, carried forward by a clock nothing here can set.** Setting the wall clock
  either way changes nothing, a suspended machine has been away, and a process restart loses nothing.
  After a reboot nobody can say how long it was off, so no new work starts until the platform answers
  or a person vouches at the console. A clock that ran ahead no longer locks a gateway out of signing.
  `clock_drift_ppm` keeps a slow crystal from stretching a lease.
- **Every command is checked the moment before it goes**, not only when it was planned: its deadline,
  the lease, the authority, a cancel, the device and what it may do. Fencing is checked in the same
  statement that writes the intent. A command out of time is held back, where it used to go with a
  second to live.
- **A dependency needs a recorded completion.** Absent, failed, started and unreadable are not done.
  How a command ended outlives its row, and what only the platform knows is asked of it.
- **A redirect cannot leave the allow list**, for firmware, updates, exports and HTTP devices, and
  encrypted never becomes plain. `allow` now means the host it names, or a range when it ends in a dot
  or a dash: `plant.example.com` used to match `plant.example.com.elsewhere.org`, and `192.168.1` used to
  match `192.168.100.5`. **Check your `allow` list.** A release's download is held to it too, so a site
  with one lists where its releases come from, redirects included. The link to the platform follows no
  redirect at all.
- A box behind a peer takes its deadline from the site that holds authority, refuses what is out of time
  or not what was authorized, and will not speak for what went out on something else.
- The console asks for the load as well as the place, when a task names one, and takes a person's word
  for the time when there is no platform to ask.
- Definitions can send `{expires_at}` and `{epoch}`, so equipment that can check them, can.

## 0.3.1

- The gateway's releases are static, so they run on any 64-bit Linux. Those of 0.3.0 needed glibc 2.39 and
  would not start on Ubuntu 22.04, Debian 12 or Raspberry Pi OS Bookworm. Names now come from `/etc/hosts`
  and DNS alone, so a `.local` name found by mDNS needs an address, or a build of your own. On ARM the
  program sits at a fixed address, as Rust's static ARM target builds it, so it gets no address
  randomization of its own; a build of your own keeps that too.
- A static build uses mimalloc, because on musl's own allocator a gateway stored about 15% fewer readings
  at full load. On one 4-core ARM machine it carries what 0.3.0 did, about 90,000 readings a second into
  the journal. Five minutes at 60,000 a second settled at about 40 MB of memory, as 0.3.0 does, after a
  peak of 78 MB rather than 52.
- A release is started on Ubuntu 22.04, Debian 12, CentOS 7 and Alpine, and passes the gateway's tests on
  the C library it ships with, before anything is published. Both run on a pull request that changes
  the release, so a tag is not the first time they are tried.

## 0.3.0

- Customers who own devices: their people see those devices, their alarms and the dashboards given to
  them in the workspace's portal, and nothing else.
- A device can show its customer only some of its readings, and alarms can be chosen for push.
- Relations between devices, by any name, shown on both and usable to choose them.
- Reports by the clock, by email, as CSV or PDF.
- Pages hear on the platform's socket that readings landed, and ask again at once.
- A workspace's allowance of measurements a day: past it, gateways hold readings until there is room.
- CoAP and LwM2M over DTLS, by pre-shared key, so they can leave a private network: `listen+coaps` and
  `listen+lwm2ms`. Met Eclipse Leshan's client.
- SNMP v3 traps, with `?user=` on `listen+snmp`, and BACnet's `discover`, by Who-Is.
- LwM2M writes go as TLV, the one format every device has to take.
- OCPP security profile 3: a charger known by its certificate. Charging profiles go as commands.
- Templates for The Things Stack, Sigfox, LORIOT, ThingPark, AWS IoT Core and Azure Event Hubs. `bytes:`
  decodes a payload delivered inside JSON, and `device:` can come from the message.
- An export to a table of your own in PostgreSQL.
- A backlog drains as fast as the platform takes it, rather than 5,000 readings each `sync_secs`, and a
  link piling up wakes the reader, rather than dropping them until its next `read_secs`. One gateway went
  from about 1,000 stored a second to 28,500 with nothing lost. `scripts/soak.py` measures it.
- AAS Part 2's repository, reading only, and what scanners saw as a GS1 EPCIS 2.0 document.
- `ferrum-site release-key` and `sign`, and a workflow that builds the gateway on a tag.
- ThingsBoard's own Python client works against a gateway unchanged, in CI, for devices and gateways.
  It found a real fault: its firmware subscription did not match the chunks, so a download hung.
- Modbus TCP against pymodbus, and Sparkplug payloads against Eclipse Tahu's definitions, in CI.
- A gateway as an OPC UA server, by an export to `listen+opcua://`: each device a folder, each metric a
  variable, read only, with a login over encryption if a secret is named.
- OPC UA history with `history:`, and events carrying `EventId` and `ConditionId`, met against asyncua.
- DNP3 as a master over TCP: integrity polls, events and unsolicited responses, and commands selected
  and then operated. It has met opendnp3's outstation.
- DLMS/COSEM meters by OBIS code, over the wrapper, read only.
- Kafka topics and AMQP queues as sources. A Kafka reader keeps its place across restarts in the
  gateway's state folder.

Upgrading: gateways first if a workspace will have an allowance of measurements, since an older one treats
a held exchange as delivered. An export to `postgres` or `listen+opcua` needs a gateway of this version. A
site that wants `listen+opcua`, `listen+coaps` or `listen+lwm2ms` names the address under `listen`.

## 0.2.0

The gaps with ThingsBoard and the other reference products, closed or named: `docs/parity.md` goes
through each capability, and says what met someone else's software, what only met a stand-in, what is
left out on purpose, and what is still missing. **Nothing has run on physical equipment.**

- Devices that call in: MQTT 3.1 to 5, HTTP and CoAP on ThingsBoard's own topics and paths, with
  tokens, passwords and certificates, provisioning, claiming, LwM2M, and OCPP 1.6 and 2.0.1.
- Protocols by name, with `tags:` for one line each: S7, EtherNet/IP, ADS, BACnet, SNMP v1 to 3 with traps,
  KNX, IEC 60870-5-104, M-Bus, and SQL, folders, FTP, NATS and CoAP as sources. Sparkplug as a primary
  host and as an edge node, MTConnect's sample stream, and browsing and events on OPC UA. CAN and
  Bluetooth by external drivers that ship here.
- Numbers worked out at the gateway, with `derive`.
- Flows: fifteen more kinds of step, answers to devices, live watching of real messages, flows on the
  platform that see across sites, one flow used inside another, every version kept, and flows in files.
- Dashboards and history, opening one another, and shared by a link that can read only what was laid out.
- Alarms that are assigned, shelved and commented on, and notices that escalate in steps.
- A fleet: find by tag, site, definition and asset, import from CSV, ask many at once, schedules,
  reports, solutions in one file, and firmware rollouts that stop by themselves, for sensors and
  meters only, and never a robot, a controller, a machine or a fleet.
- Exports to NATS, Kafka, RabbitMQ, InfluxDB, ClickHouse, S3, SQS, SNS, Pub/Sub, Azure IoT Hub, Event
  Hubs and Sparkplug. An address with a password in it is refused.
- A device as a twin, a W3C Thing Description and an Asset Administration Shell.
- Tighter limits: every listener needs the site's `listen` list, a definition reads files only under
  `files_dir`, and a poll that writes is refused, because writing is a command.

Upgrading needs a gateway and the app together: `ConfigRequest` and `FlowSpec` grew, in ways an older
one ignores. A site that wants `file://` or `sqlite://` must name `files_dir`.

## 0.1.0

IoT Edge in a repository of its own, with the gateway it runs on: the app, `ferrum-site` and what the
two share moved here from Ferrum with their history. Workspaces keep the app as `edge`, and nothing
about their data changes. Run against simulators and stand-ins only.

- Definitions: devices described in a few lines of YAML over TCP, UDP, serial, HTTP, WebSocket, MQTT,
  Modbus TCP and RTU, OPC UA, listeners and webhooks, reading JSON, XML and Sparkplug B, with twenty
  templates, and anything else through an external driver.
- Flows: logic drawn on a canvas, simulated, reviewed by someone else, and run at the gateway with or
  without the link. A flow can ask for a command and cannot give one.
- Gateways: `ferrum-site` keeps a journal through outages, crashes and restores, and runs on what it
  was last told.
- Server to server: boxes in cabinets, a relay out of a closed network, exports held until delivered,
  webhooks, and signed updates with rollback.
- Alarms, reported state, a robot map and a console on the gateway.
