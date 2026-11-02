# Definitions

A definition says how a kind of device is reached, read and commanded, as YAML. There is no code in
one, so it can be reviewed, versioned, tried on a sample with no device attached, and sent to gateways
by the platform. One engine runs them all. What cannot be said this way runs as an external driver.

```yaml
name: acme-scale
connect: tcp://10.0.0.7:9100
frame: {line: "\r\n"}
poll:
  - {every: 5s, send: "W\r\n"}
read:
  - {match: '^(?P<kg>-?[0-9.]+) kg$', metric: weight.kg, group: kg, unit: kg, deadband: 0.01}
commands:
  scale.tare:
    send: "T\r\n"
    status: {match: '^(?P<r>OK|ERR)', group: r, done: OK, failed: ERR}
```

The interface offers twenty templates to start from, in `templates`. A template is a
start and not a promise: none has been tried on the equipment it names ([compatibility](compatibility.md)).

## Connecting

| Address | What it is |
|---|---|
| `tcp://host:port` | A stream of bytes |
| `udp://host:port` | Datagrams to and from one address |
| `serial:///dev/ttyUSB0?baud=9600&parity=E&stop=1&bits=8` | A serial port. Parity is N, E or O |
| `http://host/base`, `https://…` | Requests, one at a time. `headers` go with each |
| `ws://host/path`, `wss://…` | A WebSocket |
| `mqtt://user:pw@host:1883?client=id`, `mqtts://…` | MQTT 5. `subscribe` lists the topics to hold open |
| `modbus+tcp://host:502?unit=1` | Modbus registers |
| `modbus+rtu:///dev/ttyUSB0?baud=9600&parity=E&unit=1` | Modbus on a serial line, shared by several units |
| `opcua://host:4840/path?interval=500` | OPC UA. `subscribe` lists the nodes to watch, and `events:i=2253` a node's events. `node: browse:i=85` says what is under one |
| `s7://plc:102?rack=0&slot=1` | Siemens S7: `DB1.DBD0:real`, `MW10`, `M0.1`, `I0.1` |
| `enip://plc:44818?slot=0` | EtherNet/IP, by tag: `Line.Speed`, `Temps[3]` |
| `ads://plc:48898?target=…&port=851&source=…` | TwinCAT ADS, by name and type: `MAIN.speed:real` |
| `bacnet://controller:47808?priority=16` | BACnet/IP: `analog-input:1`, `binary-value:2/present-value` |
| `snmp://device:161?community=public&version=2c` | SNMP v1 and v2c: an OID, or `walk:` and one |
| `listen+snmp://0.0.0.0:162` | Traps and informs |
| `knx://interface:3671` | KNX over IP. `subscribe` and `node` say `1/2/3:dpt9` |
| `iec104://rtu:2404?ca=1` | IEC 60870-5-104: `interrogate`, `single:4001`, `double:4002`, `setpoint:4003` |
| `mbus://bridge:9000` | M-Bus meters through a TCP bridge. `node` is a primary address |
| `coap://sensor:5683` | A device that serves CoAP. `get`, `put`, `post`, `delete`, and `subscribe` to watch with Observe |
| `listen+lwm2m://0.0.0.0:5683` | LwM2M devices register here. `subscribe` lists what to watch on each |
| `listen+ocpp://0.0.0.0:9000/ocpp?open=1`, `listen+ocpps://…` | EV chargers, with `api: ocpp`. `open=1` lets a charger with no token in |
| `mtconnect://agent:5000?interval=1000` | An MTConnect agent: everything, then its sample stream |
| `nats://bus:4222` | NATS subjects in `subscribe` |
| `postgres://user:pw@host/db`, `sqlite:///path` | A query in `node`, read only |
| `file:///data/out?every=30&match=.csv` | New files in a folder, line by line |
| `ftp://user:pw@host/out?every=60` | The same on an FTP server |
| `listen://0.0.0.0:5020` | Equipment calls in over TCP |
| `listen+udp://0.0.0.0:5020` | Equipment sends datagrams here |
| `webhook://0.0.0.0:8088?token={secret.x}` | Another system posts here |
| `process://name --args` | An external driver |

A device can give its own address, which is how one definition serves a hundred devices. Each distinct
address is one connection, shared by the devices on it, and it reconnects on its own, backing off.

A definition can come from the platform, so the site decides what one may do there (`site.yaml`):

- `allow` limits hosts, by what they start with. Empty means any.
- `listen` names addresses a definition may listen on, exactly. Empty means none.
- `drivers_dir` is the only place a driver can be run from.
- `files_dir` is the only place a definition can read files and SQLite databases from.
- A serial port must be under `/dev`.
- `tls: {ca, cert, key}` names files in the site's `state_dir/tls`, and nowhere else. It works for
  `https` and `mqtts`. `wss` uses public authorities only.

OPC UA takes `security=Basic256Sha256&mode=SignAndEncrypt` and `user=…&password=…`. With security on,
the site makes its own certificate under `state_dir/tls/opcua`, and a server is believed only once a
person has moved its certificate from `rejected` to `trusted` there.
The RSA behind this comes from a library with an unfixed timing weakness ([the record](advisories.yaml)),
so keep OPC UA on a segmented network.

Whatever reads by a name inside the equipment says it the same way, with `tags`, which is the short
way to say what most equipment needs:

```yaml
name: oven
connect: s7://10.0.0.7:102?rack=0&slot=1
tags:
  - {node: "DB1.DBD0:real", every: 1s, metric: temp.c, unit: C, deadband: 0.2}
  - {node: DB1.DBX4.0, every: 1s, state: true, map: {"true": Running, "false": Idle}}
```

Each becomes a poll and a read rule. A wrong address is refused when a definition is saved, and a poll
only ever reads: writing to equipment is a command, and goes through the ledger.

`primary_host: an-id` on a Sparkplug definition makes the link a primary host: it says it is there on
`spBv1.0/STATE/an-id`, follows each node's sequence, asks for a rebirth after a gap, and says when a
node or device goes away.

## Cutting a stream into messages

Datagrams, MQTT and HTTP are messages already. For a stream, `frame` says where one ends:

```yaml
frame: {line: "\r\n"}                                    # ends with this
frame: {fixed: 16}                                       # exactly this long
frame: {between: ["\x02", "\x03"]}                       # between two markers
frame: {length: {start: "AA 55", at: 2, size: 1, add: 5}} # a marker, then a length field
```

`add` turns the field's value into the whole message's length: the header and any check. `little:
true` reads the field least significant first. What cannot be a message is thrown away rather than
held, so a stream that went wrong finds its feet.

`checksum: {kind: crc16-modbus, from: 1}` is added to what is sent as bytes and checked on what
arrives, and a message that fails is counted and dropped. Kinds: `crc16-modbus`, `crc16-ccitt`,
`crc32`, `sum8`, `xor8`, `lrc`. It covers the message from `from`, for a header it leaves out.

## Reading

Each rule under `read` is tried on every message. `topic` narrows it, with MQTT's `+` and `#`, to a
topic, an HTTP path or an OPC UA node. `match` is a regular expression, and `when` a condition:
`{json: $.topic, is: /battery_state}`, or `{at: 3, is: "01"}` for bytes. A rule says one value, or
several under `values`.

A value says what it is, once: `metric: name` (a number to record), `state: true` (Running, Idle,
Stopped, Maintenance or Unknown), `attribute: name` (a fact that changes rarely) or `event: name`
(something that happened).

It says where it comes from, once: `json: $.a.b[0]`, `group: name` (of `match`), `at: 4` (a byte, with
`type`, `order` and `len`), `reg: 2` (a Modbus register, counted from the read), or `value:` (fixed:
this rule matching is the news). With none of those it is the whole message, as text.

`type` is `u8` to `u64`, `i8` to `i64`, `f32`, `f64`, `text` or `hex`. `order` is `be`, `le`, `cdab` or
`badc`, because PLCs split words in every way there is. Then, in this order: `bit: 3` takes one bit,
`map: {0: Idle, 1: Running}` says what a raw value means, and `scale`, `offset` and `round` do
arithmetic. `unit` and `class` (`Sample`, `State` or `Alarm`: what is given up last when space runs
out) are kept with it.

`deadband: 0.5` reports a number only when it has moved that far, and `every: 10s` at most that
often. `time: $.timestamp` keeps the time the equipment gave, as RFC 3339 or seconds or milliseconds
since 1970.

`device: "{topic.1}"` says whose message this is, when several devices share a connection, from a
`+` in the topic, `{group.x}` or `{each.x}`. A code nobody has added goes on the list of strangers for
a person to claim, and nothing it says is recorded until then.

### Formats

`format: sparkplug` reads a Sparkplug B payload as `{timestamp, seq, metrics: [{name, alias, datatype,
value}]}`, with a metric's name filled in from its alias by the node's last birth certificate. A
metric whose alias nothing explained has no name and is not recorded under a wrong one. `format: xml`
reads every element with no children, as a flat list: `[{tag, parent, text, attrs, of}]`, where `of`
holds the attributes of the elements around it by their names.

### Many values at once

`each: $.metrics` applies the rule to each element of a list, or `{key, value}` of a map. Paths then
start at the element, `when` is asked of each one, and a value's name can come from it:

```yaml
read:
  - {topic: spBv1.0/plant/+/+/+, format: sparkplug, device: "{topic.3}", each: $.metrics, metric: "{each.name}", json: $.value}
```

This makes a device with hundreds of values one line. A name a device chose is made fit to be a
metric's, and one link keeps track of at most 20,000 of them.

## Numbers worked out from others

```yaml
derive:
  - {metric: power.kw, formula: "[volts] * [amps] / 1000", unit: kW}
  - {metric: cost.hour, formula: "[power.kw] * 0.2", fresh: 30s}
```

A derived number is worked out at the gateway whenever one of its inputs is reported, once all of
them have been heard, so it works with the link down. Each can use those listed before it. An input
older than `fresh` does not count: a stale reading is not a reading.

## Commands

The name under `commands` is the operation a device is commissioned for. A step says what to send:

| On | Step |
|---|---|
| Streams, datagrams, WebSocket | `send: text or data`, `hex: "01 02"` |
| HTTP | `get`, `post`, `put`, `patch` or `delete: /path`, with `send` as the body |
| MQTT | `topic: t`, `send:`, `retain: true`, `format: sparkplug` |
| Modbus | `read: holding`, `at`, `count`, `write: [{at, value}]`, `unit` |
| OPC UA | `node: id` to read it, with `send` to write it, and `call: method` to call |

Text with `{placeholders}` is filled in: `command`, `device`, `id` (the equipment's name for the
work), `now`, `now_ms`, `seq`, `payload.x`, `settings.x` and `secret.x`. A placeholder with nothing
behind it refuses the command: half a command must not reach a machine. A string that is one
placeholder keeps its value's type, so a job number stays a number.

A value written to OPC UA takes the node's current type. One that does not fit is refused rather than
rounded. Arguments to a method say their type: `{type: int32, value: "{payload.job}"}`.

A command must say how it ends:

```yaml
status:
  topic: agv/{device}/state     # or poll: {get: /jobs/{id}}, to ask
  id: $.orderId                 # the answer names its command
  json: $.state
  running: [EXECUTING]
  done: FINISHED
  failed: FAILED
  aborted: CANCELLED
  also:                         # for a protocol with no one field for it
    - {topic: agv/{device}/state, id: $.orderId, json: $.nodes, done: "[]"}
```

`id: $.id` on the command itself says where the reply gives the equipment's name for the work. `find`
asks the equipment whether it has heard of a command after a crash, only for equipment that keeps its
own record: a "no" makes sending it again safe. `cancel` is how to stop it, and whether it stopped is
for the status to say. `once: true` admits that this device can prove nothing beyond the send.

A definition promises no more than an adapter may: a command is accepted once it has gone, ends only
when its status says so, and a write that was not answered leaves it unknown, for a person to settle.

## External drivers

A program in `drivers_dir`, in any language, speaks one JSON object to a line. With no `read` in its
definition it says:

```json
{"metric": "temp.c", "value": 21.5, "device": "PRESS-1", "unit": "C"}
{"state": "Running", "device": "PRESS-1"}
{"attribute": "firmware", "value": "1.2", "device": "PRESS-1"}
{"event": "door.open", "value": true, "device": "PRESS-1"}
{"command": "<id>", "status": "running"}
```

`status` is `running`, `done`, `failed` or `aborted`. It hears:

```json
{"op": "dispatch", "command": "<id>", "operation": "machine.start", "device": "PRESS-1", "payload": {}, "settings": {}, "expires_at": "…"}
{"op": "cancel", "command": "<id>"}
```

It runs as a process of its own, with what it writes to stderr in the site's log, so a vendor's
library or a fieldbus stack can crash without taking the runtime with it. A driver that has a
definition's `read` can say anything, and the definition interprets it.

## Trying one

`POST /api/app/edge/definitions/try` reads a sample by a definition, by the code a gateway runs. The
interface does this as you type a sample. Publishing is what reaches gateways, so a draft breaks
nothing, and each published revision replaces only the connections it changes.
