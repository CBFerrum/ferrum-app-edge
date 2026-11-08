import { t } from "@ferrum/client";
// What a flow is, as the canvas sees it. The node kinds mirror `ferrum_edge_contract::flow`, which is
// what decides whether a flow is any good: this only helps a person draw one.

export interface FlowNode {
  id: string;
  kind: string;
  x: number;
  y: number;
  label?: string;
  [param: string]: unknown;
}

export interface FlowEdge {
  from: string;
  port?: string;
  to: string;
  input?: string;
}

export interface Flow {
  nodes: FlowNode[];
  edges: FlowEdge[];
  /** `critical`: a reading it has had changes nothing, and its gateway writes where it stands with what it did after each one. */
  profile?: "critical";
}

export interface Param {
  name: string;
  label: string;
  type: "text" | "number" | "duration" | "select" | "check" | "json" | "list";
  hint?: string;
  options?: string[];
  required?: boolean;
  placeholder?: string;
}

export type Group = "Listen" | "Decide" | "Do";

export interface Kind {
  kind: string;
  label: string;
  group: Group;
  says: string;
  params: Param[];
  /** Ways out. A switch has one per case, so it works that out from its own settings. */
  ports: string[];
  /** Ways in, or null for inputs named by whoever wires them. */
  inputs: string[] | null;
  defaults?: Record<string, unknown>;
}

const device: Param = { name: "device", get label() { return t("Device"); }, type: "text", get placeholder() { return t("Any device"); }, get hint() { return t("A code, or the start of one and a star: PRESS-*"); } };
const yesNo = ["yes", "no"];
const out = ["out"];
const across: Param = { name: "across", get label() { return t("Across devices"); }, type: "check", get hint() { return t("Otherwise each device is taken on its own"); } };
const fresh: Param = { name: "fresh", get label() { return t("Ignore values older than"); }, type: "duration", get placeholder() { return t("Never"); }, get hint() { return t("A stale reading is not a reading"); } };

export const KINDS: Kind[] = [
  {
    kind: "metric",
    get label() { return t("A number"); },
    group: "Listen",
    get says() { return t("A number a device reported"); },
    params: [{ name: "metric", get label() { return t("Value"); }, type: "text", required: true, placeholder: "temp.c" }, device],
    ports: out,
    inputs: [],
    defaults: { metric: "" },
  },
  { kind: "state", get label() { return t("A state change"); }, group: "Listen", get says() { return t("A device starting, stopping or going idle"); }, params: [device], ports: out, inputs: [] },
  {
    kind: "event",
    get label() { return t("An event"); },
    group: "Listen",
    get says() { return t("Something a device said happened"); },
    params: [{ name: "name", get label() { return t("Event"); }, type: "text", required: true }, device],
    ports: out,
    inputs: [],
    defaults: { name: "" },
  },
  {
    kind: "attribute",
    get label() { return t("An attribute"); },
    group: "Listen",
    get says() { return t("A setting or fact about a device changing"); },
    params: [{ name: "key", get label() { return t("Attribute"); }, type: "text", required: true }, device],
    ports: out,
    inputs: [],
    defaults: { key: "" },
  },
  {
    kind: "input",
    get label() { return t("Way into this flow"); },
    group: "Listen",
    get says() { return t("For a flow other flows use: where they come in. On its own, a number of this name comes in here"); },
    params: [{ name: "name", get label() { return t("Name"); }, type: "text", required: true }],
    ports: out,
    inputs: [],
    defaults: { name: "in" },
  },
  {
    kind: "timer",
    get label() { return t("A timer"); },
    group: "Listen",
    get says() { return t("Fires on its own, every so often"); },
    params: [{ name: "every", get label() { return t("Every"); }, type: "duration", required: true }],
    ports: out,
    inputs: [],
    defaults: { every: "1m" },
  },
  {
    kind: "compare",
    get label() { return t("Compare"); },
    group: "Decide",
    get says() { return t("Sends it one way or the other"); },
    params: [
      { name: "op", get label() { return t("Is"); }, type: "select", options: [">", ">=", "<", "<=", "==", "!="], required: true },
      { name: "value", get label() { return t("Than"); }, type: "json", required: true, get hint() { return t("A number or some text"); } },
    ],
    ports: yesNo,
    inputs: ["in"],
    defaults: { op: ">", value: 0 },
  },
  {
    kind: "range",
    get label() { return t("In range"); },
    group: "Decide",
    get says() { return t("Between two numbers or outside them"); },
    params: [
      { name: "min", get label() { return t("From"); }, type: "number", required: true },
      { name: "max", get label() { return t("To"); }, type: "number", required: true },
    ],
    ports: yesNo,
    inputs: ["in"],
    defaults: { min: 0, max: 100 },
  },
  {
    kind: "hold",
    get label() { return t("Must last"); },
    group: "Decide",
    get says() { return t("Passes once it has lasted this long. One spike is not an alarm."); },
    params: [{ name: "for", get label() { return t("For"); }, type: "duration", required: true }],
    ports: out,
    inputs: ["in", "reset"],
    defaults: { for: "30s" },
  },
  {
    kind: "silence",
    get label() { return t("Gone quiet"); },
    group: "Decide",
    get says() { return t("Fires when nothing has arrived for this long"); },
    params: [{ name: "for", get label() { return t("For"); }, type: "duration", required: true }],
    ports: out,
    inputs: ["in"],
    defaults: { for: "5m" },
  },
  {
    kind: "changed",
    get label() { return t("Only changes"); },
    group: "Decide",
    get says() { return t("Lets through what is different from last time"); },
    params: [{ name: "by", get label() { return t("By at least"); }, type: "number", get placeholder() { return t("Any change"); } }],
    ports: out,
    inputs: ["in"],
  },
  {
    kind: "limit",
    get label() { return t("Not too often"); },
    group: "Decide",
    get says() { return t("At most one message this often"); },
    params: [{ name: "every", get label() { return t("Every"); }, type: "duration", required: true }],
    ports: out,
    inputs: ["in"],
    defaults: { every: "1m" },
  },
  {
    kind: "delay",
    get label() { return t("Wait"); },
    group: "Decide",
    get says() { return t("Passes each message on later"); },
    params: [{ name: "for", get label() { return t("For"); }, type: "duration", required: true }],
    ports: out,
    inputs: ["in"],
    defaults: { for: "10s" },
  },
  {
    kind: "count",
    get label() { return t("Count"); },
    group: "Decide",
    get says() { return t("Passes when this many have arrived in a time"); },
    params: [
      { name: "of", get label() { return t("How many"); }, type: "number", required: true },
      { name: "within", get label() { return t("Within"); }, type: "duration", required: true },
    ],
    ports: out,
    inputs: ["in"],
    defaults: { of: 3, within: "1m" },
  },
  {
    kind: "window",
    get label() { return t("Average over time"); },
    group: "Decide",
    get says() { return t("An average, highest, lowest or total of the last while"); },
    params: [
      { name: "stat", get label() { return t("Work out"); }, type: "select", options: ["avg", "min", "max", "sum", "count", "delta"], required: true },
      { name: "over", get label() { return t("Over"); }, type: "duration", required: true },
    ],
    ports: out,
    inputs: ["in"],
    defaults: { stat: "avg", over: "1m" },
  },
  {
    kind: "math",
    get label() { return t("Calculate"); },
    group: "Decide",
    get says() { return t("Works something out from the value"); },
    params: [{ name: "formula", get label() { return t("Formula"); }, type: "text", required: true, get placeholder() { return t("[value] * 1.8 + 32"); }, get hint() { return t("+ - * / round abs min max"); } }],
    ports: out,
    inputs: ["in"],
    defaults: { formula: "[value]" },
  },
  {
    kind: "combine",
    get label() { return t("Combine"); },
    group: "Decide",
    get says() { return t("Waits for every input, then passes them together"); },
    params: [{ name: "formula", get label() { return t("Formula"); }, type: "text", placeholder: "[a] * [b]", get hint() { return t("Inputs by the names on their wires"); } }, fresh, across],
    ports: out,
    inputs: null,
  },
  {
    kind: "all",
    get label() { return t("All true"); },
    group: "Decide",
    get says() { return t("Yes when every input is true, and no when that stops"); },
    params: [{ name: "any", get label() { return t("One is enough"); }, type: "check" }, fresh, across],
    ports: yesNo,
    inputs: null,
  },
  { kind: "latch", get label() { return t("Remember"); }, group: "Decide", get says() { return t("Stays on from set until reset"); }, params: [], ports: yesNo, inputs: ["set", "reset"] },
  {
    kind: "map",
    get label() { return t("Translate"); },
    group: "Decide",
    get says() { return t("What a raw value means: 0 is Idle, 1 is Running"); },
    params: [
      { name: "map", get label() { return t("Meanings"); }, type: "json", required: true, hint: '{"0": "Idle", "1": "Running"}' },
      { name: "other", get label() { return t("Otherwise"); }, type: "json", get placeholder() { return t("Drop it"); } },
    ],
    ports: out,
    inputs: ["in"],
    defaults: { map: {} },
  },
  {
    kind: "switch",
    get label() { return t("Choose by value"); },
    group: "Decide",
    get says() { return t("One way out for each value"); },
    params: [
      { name: "cases", get label() { return t("Values"); }, type: "list", required: true, get hint() { return t("One to a line"); } },
      { name: "on", get label() { return t("By the part at"); }, type: "text", get placeholder() { return t("The whole value"); }, get hint() { return t("$.method keeps the rest for what follows"); } },
    ],
    ports: ["other"],
    inputs: ["in"],
    defaults: { cases: ["Running", "Stopped"] },
  },
  {
    kind: "set",
    get label() { return t("Replace"); },
    group: "Decide",
    get says() { return t("Passes something else instead"); },
    params: [{ name: "value", get label() { return t("With"); }, type: "json", required: true, hint: "{value} and {device} work here" }],
    ports: out,
    inputs: ["in"],
    defaults: { value: "" },
  },
  {
    kind: "field",
    get label() { return t("Take a part"); },
    group: "Decide",
    get says() { return t("One thing out of a value with parts"); },
    params: [{ name: "path", get label() { return t("Path"); }, type: "text", required: true, placeholder: "$.a.b" }],
    ports: out,
    inputs: ["in"],
    defaults: { path: "" },
  },
  {
    kind: "has",
    get label() { return t("Has these parts"); },
    group: "Decide",
    get says() { return t("Whether a value has what is needed"); },
    params: [{ name: "paths", get label() { return t("Parts"); }, type: "list", required: true, get hint() { return t("One to a line: $.a.b"); } }],
    ports: yesNo,
    inputs: ["in"],
    defaults: { paths: [] },
  },
  {
    kind: "delta",
    get label() { return t("How far it went"); },
    group: "Decide",
    get says() { return t("A counter's rise since last time. One that went backwards was reset, and says nothing."); },
    params: [{ name: "per", get label() { return t("As a rate per"); }, type: "duration", get placeholder() { return t("Plain difference"); } }],
    ports: out,
    inputs: ["in"],
  },
  {
    kind: "geofence",
    get label() { return t("In a place"); },
    group: "Decide",
    get says() { return t("Inside or outside a circle or a polygon, and when that changes"); },
    params: [
      { name: "lat", get label() { return t("Latitude at"); }, type: "text", get placeholder() { return t("$.lat"); } },
      { name: "lon", get label() { return t("Longitude at"); }, type: "text", get placeholder() { return t("$.lon"); } },
      { name: "circle", get label() { return t("Circle"); }, type: "json", hint: '{"lat": 51.5, "lon": -0.12, "radius_m": 500}' },
      { name: "polygon", get label() { return t("Or a polygon"); }, type: "json", get hint() { return t("[[lat, lon], [lat, lon], [lat, lon]]"); } },
    ],
    ports: ["inside", "outside", "entered", "left"],
    inputs: ["in"],
    defaults: { circle: { lat: 0, lon: 0, radius_m: 100 } },
  },
  {
    kind: "dedupe",
    get label() { return t("Only the first"); },
    group: "Decide",
    get says() { return t("Lets the first through and not the same again for a while"); },
    params: [
      { name: "within", get label() { return t("For"); }, type: "duration", required: true },
      { name: "key", get label() { return t("The same by"); }, type: "text", get placeholder() { return t("The whole value"); }, get hint() { return t("A part, like $.id"); } },
    ],
    ports: out,
    inputs: ["in"],
    defaults: { within: "1m" },
  },
  {
    kind: "split",
    get label() { return t("One for each"); },
    group: "Decide",
    get says() { return t("A message for each thing in a list"); },
    params: [{ name: "path", get label() { return t("The list at"); }, type: "text", get placeholder() { return t("The value itself"); } }],
    ports: out,
    inputs: ["in"],
  },
  {
    kind: "keys",
    get label() { return t("Rename parts"); },
    group: "Decide",
    get says() { return t("Renames the parts of a value and leaves some out"); },
    params: [
      { name: "rename", get label() { return t("Rename"); }, type: "json", hint: '{"old": "new"}' },
      { name: "drop", get label() { return t("Leave out"); }, type: "list", get hint() { return t("One to a line"); } },
      { name: "keep", get label() { return t("Keep only"); }, type: "list", get hint() { return t("One to a line"); } },
    ],
    ports: out,
    inputs: ["in"],
  },
  {
    kind: "enrich",
    get label() { return t("Add what is known"); },
    group: "Decide",
    get says() { return t("Adds a device's newest numbers and what it said about itself"); },
    params: [
      { name: "metrics", get label() { return t("Numbers"); }, type: "list", get hint() { return t("One to a line"); } },
      { name: "attributes", get label() { return t("Attributes"); }, type: "list", get hint() { return t("One to a line"); } },
      { name: "device", get label() { return t("Of device"); }, type: "text", get placeholder() { return t("The one it came from"); } },
    ],
    ports: out,
    inputs: ["in"],
  },
  {
    kind: "schedule",
    get label() { return t("At these times"); },
    group: "Decide",
    get says() { return t("Yes between two times of day, on these days"); },
    params: [
      { name: "from", get label() { return t("From"); }, type: "text", required: true, placeholder: "08:00" },
      { name: "to", get label() { return t("Until"); }, type: "text", required: true, placeholder: "18:00" },
      { name: "days", get label() { return t("Days"); }, type: "json", get hint() { return t("1 is Monday: [1, 2, 3, 4, 5]. Every day, if empty"); } },
      { name: "offset_minutes", get label() { return t("Minutes from UTC"); }, type: "number", get hint() { return t("The site's time zone: 330 for India"); } },
    ],
    ports: yesNo,
    inputs: ["in"],
    defaults: { from: "08:00", to: "18:00" },
  },
  {
    kind: "normalize",
    get label() { return t("Bring to units"); },
    group: "Decide",
    get says() { return t("Raw counts to engineering units, exactly: value × scale + offset. What is not a number, or cannot be, goes to quarantine"); },
    params: [
      { name: "scale", get label() { return t("Scale"); }, type: "text", placeholder: "0.1" },
      { name: "offset", get label() { return t("Offset"); }, type: "text", placeholder: "-40" },
      { name: "unit", get label() { return t("Unit"); }, type: "text", placeholder: "°C" },
      { name: "min", get label() { return t("Lowest it can be"); }, type: "text" },
      { name: "max", get label() { return t("Highest it can be"); }, type: "text" },
    ],
    ports: ["out", "quarantine"],
    inputs: ["in"],
    defaults: { scale: "1" },
  },
  {
    kind: "quality",
    get label() { return t("By quality"); },
    group: "Decide",
    get says() { return t("Sends a reading on by how far it can be trusted: good, uncertain, bad, or stale when it was seen too long ago"); },
    params: [{ name: "stale", get label() { return t("Stale after"); }, type: "duration", get placeholder() { return t("Never"); } }],
    ports: ["good", "uncertain", "bad", "stale"],
    inputs: ["in"],
  },
  {
    kind: "event_window",
    get label() { return t("Average over the time seen"); },
    group: "Decide",
    get says() { return t("A statistic for each stretch of the time readings were seen, not when they arrived. A late one corrects it on revised; later still is late"); },
    params: [
      { name: "size", get label() { return t("Each window"); }, type: "duration", required: true },
      { name: "stat", get label() { return t("Statistic"); }, type: "select", options: ["avg", "min", "max", "sum", "count", "delta"], required: true },
      { name: "wait", get label() { return t("Wait for stragglers"); }, type: "duration", get hint() { return t("How long after a window's end it waits before saying anything") } },
      { name: "lateness", get label() { return t("Take corrections for"); }, type: "duration" },
      { name: "idle", get label() { return t("A quiet device closes after"); }, type: "duration", get placeholder() { return t("Never"); } },
      across,
    ],
    ports: ["out", "revised", "late"],
    inputs: ["in"],
    defaults: { size: "1m", stat: "avg" },
  },
  {
    kind: "episode",
    get label() { return t("An episode"); },
    group: "Decide",
    get says() { return t("A condition that holds, with one identity until it clears. Only a good reading moves it; silence is not recovery"); },
    params: [
      { name: "above", get label() { return t("Holds above"); }, type: "number" },
      { name: "below", get label() { return t("Holds below"); }, type: "number" },
      { name: "close", get label() { return t("Clears past"); }, type: "number", required: true, get hint() { return t("The gap between where it holds and where it clears is a deadband") } },
      { name: "on_delay", get label() { return t("Must hold for"); }, type: "duration" },
      { name: "off_delay", get label() { return t("Must stay clear for"); }, type: "duration" },
      { name: "reopen", get label() { return t("Same episode if it holds again within"); }, type: "duration" },
      { name: "stale", get label() { return t("Say it went quiet after"); }, type: "duration" },
    ],
    ports: ["opened", "reopened", "evidence", "stale", "closed", "interrupted"],
    inputs: ["in"],
    defaults: { above: 80, close: 75, on_delay: "30s", off_delay: "1m" },
  },
  {
    kind: "anomaly",
    get label() { return t("Unlike the rest"); },
    group: "Decide",
    get says() { return t("A number unlike the last ones. It learns each device's normal, and an odd one does not teach it."); },
    params: [
      { name: "window", get label() { return t("Readings to compare with"); }, type: "number", placeholder: "100" },
      { name: "sigma", get label() { return t("Standard deviations"); }, type: "number", placeholder: "3" },
    ],
    ports: ["normal", "odd"],
    inputs: ["in"],
  },
  {
    kind: "use",
    get label() { return t("Use a flow"); },
    group: "Decide",
    get says() { return t("An approved flow's steps, copied in when this is checked. What it uses is part of what is reviewed"); },
    params: [
      { name: "flow", get label() { return t("Flow"); }, type: "text", required: true },
      { name: "inputs", get label() { return t("Its ways in"); }, type: "list", get hint() { return t("One to a line, as it names them"); } },
      { name: "outputs", get label() { return t("Its ways out"); }, type: "list", get hint() { return t("One to a line, as it names them"); } },
    ],
    ports: out,
    inputs: ["in"],
    defaults: { flow: "", inputs: ["in"], outputs: ["out"] },
  },
  {
    kind: "reassign",
    get label() { return t("Make it another's"); },
    group: "Decide",
    get says() { return t("What follows treats the message as another device's"); },
    params: [{ name: "to", get label() { return t("Device"); }, type: "text", required: true, hint: "{value} and {device} work here" }],
    ports: out,
    inputs: ["in"],
    defaults: { to: "" },
  },
  {
    kind: "alarm",
    get label() { return t("Raise an alarm"); },
    group: "Do",
    get says() { return t("Raises at the top input and clears at the other"); },
    params: [
      { name: "name", get label() { return t("Name"); }, type: "text", required: true },
      { name: "severity", get label() { return t("Severity"); }, type: "select", options: ["Info", "Warning", "Minor", "Major", "Critical"], required: true },
      { name: "device", get label() { return t("On device"); }, type: "text", get placeholder() { return t("The one it came from"); } },
    ],
    ports: [],
    inputs: ["in", "clear"],
    defaults: { name: "", severity: "Warning" },
  },
  {
    kind: "notify",
    get label() { return t("Tell someone"); },
    group: "Do",
    get says() { return t("Tells whoever looks after IoT Edge"); },
    params: [
      { name: "title", get label() { return t("Message"); }, type: "text", required: true, hint: "{value} and {device} work here" },
      { name: "every", get label() { return t("At most every"); }, type: "duration" },
    ],
    ports: [],
    inputs: ["in"],
    defaults: { title: "" },
  },
  {
    kind: "output",
    get label() { return t("Way out of this flow"); },
    group: "Do",
    get says() { return t("For a flow other flows use: where they carry on from"); },
    params: [{ name: "name", get label() { return t("Name"); }, type: "text", required: true }],
    ports: [],
    inputs: ["in"],
    defaults: { name: "out" },
  },
  {
    kind: "reply",
    get label() { return t("Answer the device"); },
    group: "Do",
    get says() { return t("Answers a device that asked something, from an event called rpc.request"); },
    params: [
      { name: "with", get label() { return t("With"); }, type: "json", hint: "{value} and {device} work here" },
      { name: "id", get label() { return t("Its question's id at"); }, type: "text", get placeholder() { return t("$.id"); } },
    ],
    ports: [],
    inputs: ["in"],
  },
  {
    kind: "log",
    get label() { return t("Write to the log"); },
    group: "Do",
    get says() { return t("For whoever is watching this flow"); },
    params: [{ name: "text", get label() { return t("Text"); }, type: "text", get placeholder() { return t("The value"); }, hint: "{value} and {device} work here" }],
    ports: [],
    inputs: ["in"],
  },
  {
    kind: "case",
    get label() { return t("Open a case"); },
    group: "Do",
    get says() { return t("Hands an episode to the platform: one condition and one case for each episode, with its evidence and how it ended"); },
    params: [{ name: "title", get label() { return t("Title"); }, type: "text", required: true, hint: "{device} works here" }],
    ports: [],
    inputs: ["in"],
    defaults: { title: "" },
  },
  {
    kind: "command",
    get label() { return t("Ask for a command"); },
    group: "Do",
    get says() { return t("Asks the ledger. It waits for a person unless an automation envelope allows it."); },
    params: [
      { name: "operation", get label() { return t("Operation"); }, type: "text", required: true, placeholder: "fan.start" },
      { name: "device", get label() { return t("Device"); }, type: "text", get placeholder() { return t("The one it came from"); } },
      { name: "payload", get label() { return t("With"); }, type: "json", hint: "{value} and {device} work here" },
      { name: "every", get label() { return t("At most every"); }, type: "duration" },
      { name: "expires", get label() { return t("Gives up after"); }, type: "duration" },
    ],
    ports: [],
    inputs: ["in"],
    defaults: { operation: "" },
  },
  { kind: "record_event", get label() { return t("Record an event"); }, group: "Do", get says() { return t("Writes it on the device's history"); }, params: [{ name: "name", get label() { return t("Event"); }, type: "text", required: true }], ports: [], inputs: ["in"], defaults: { name: "" } },
  {
    kind: "record_metric",
    get label() { return t("Record a number"); },
    group: "Do",
    get says() { return t("A new value of the device's, as if it had said it"); },
    params: [
      { name: "name", get label() { return t("Value"); }, type: "text", required: true },
      { name: "unit", get label() { return t("Unit"); }, type: "text" },
    ],
    ports: [],
    inputs: ["in"],
    defaults: { name: "" },
  },
  { kind: "record_attribute", get label() { return t("Set an attribute"); }, group: "Do", get says() { return t("Writes down a fact about the device"); }, params: [{ name: "key", get label() { return t("Attribute"); }, type: "text", required: true }], ports: [], inputs: ["in"], defaults: { key: "" } },
  { kind: "export", get label() { return t("Send elsewhere"); }, group: "Do", get says() { return t("Hands it to one of the site's exports, kept until delivered"); }, params: [{ name: "to", get label() { return t("Export"); }, type: "text", required: true }], ports: [], inputs: ["in"], defaults: { to: "" } },
];

const byKind = new Map(KINDS.map((k) => [k.kind, k]));

export const kindOf = (kind: string): Kind => byKind.get(kind) ?? { kind, label: kind, group: "Decide", says: "", params: [], ports: out, inputs: ["in"] };

export const portsOf = (n: FlowNode): string[] => (n.kind === "switch" ? [...((n.cases as string[]) ?? []), "other"] : n.kind === "use" ? ((n.outputs as string[]) ?? ["out"]) : kindOf(n.kind).ports)

/** Ways in that exist on the canvas: a combine's are whatever has been wired to it, and one more. */
export function inputsOf(flow: Flow, n: FlowNode): string[] {
  if (n.kind === "use") return (n.inputs as string[]) ?? ["in"];
  const fixed = kindOf(n.kind).inputs;
  if (fixed) return fixed;
  return [...new Set(flow.edges.filter((e) => e.to === n.id).map((e) => e.input ?? "in"))];
}

/** A name for one more wire into a step whose inputs are named by their wires. */
export function nextInput(flow: Flow, n: FlowNode): string {
  const used = inputsOf(flow, n);
  return "abcdefghijklmnopqrstuvwxyz".split("").find((c) => !used.includes(c)) ?? `in${used.length}`;
}

/** Where a wire goes when someone joins it to a step, and not to one of its dots. */
export function firstInput(flow: Flow, n: FlowNode): string | null {
  const fixed = kindOf(n.kind).inputs;
  if (fixed === null) return nextInput(flow, n);
  return fixed[0] ?? null;
}

export const NODE_W = 184;
export const NODE_H = 62;
const COLUMN = 244;
const ROW = 96;

/** Where a way in or out sits on a node. */
export function portAt(n: FlowNode, side: "in" | "out", index: number, count: number): { x: number; y: number } {
  return { x: n.x + (side === "out" ? NODE_W : 0), y: n.y + (NODE_H * (index + 1)) / (count + 1) };
}

/** A wire, as a curve that leaves and arrives level. */
export function wirePath(a: { x: number; y: number }, b: { x: number; y: number }): string {
  const bend = Math.max(36, Math.abs(b.x - a.x) / 2);
  return `M${a.x},${a.y} C${a.x + bend},${a.y} ${b.x - bend},${b.y} ${b.x},${b.y}`;
}

export function reaches(flow: Flow, from: string, to: string): boolean {
  const seen = new Set<string>();
  const walk = [from];
  while (walk.length) {
    const here = walk.pop()!;
    if (here === to) return true;
    if (seen.has(here)) continue;
    seen.add(here);
    walk.push(...flow.edges.filter((e) => e.from === here).map((e) => e.to));
  }
  return false;
}

/** Why this wire cannot be made, in words, or null. The server has the last word on all of this. */
export function whyNot(flow: Flow, e: FlowEdge): string | null {
  if (e.from === e.to) return t("A step cannot lead to itself.");
  if (reaches(flow, e.to, e.from)) return t("That would go round in a circle, and a flow cannot.");
  if (flow.edges.some((x) => x.from === e.from && (x.port ?? "out") === (e.port ?? "out") && x.to === e.to && (x.input ?? "in") === (e.input ?? "in"))) return t("They are already joined that way.");
  return null;
}

export const connect = (flow: Flow, e: FlowEdge): Flow => (whyNot(flow, e) ? flow : { ...flow, edges: [...flow.edges, e] });

export const disconnect = (flow: Flow, e: FlowEdge): Flow => ({ ...flow, edges: flow.edges.filter((x) => x !== e) });

export function remove(flow: Flow, id: string): Flow {
  return { ...flow, nodes: flow.nodes.filter((n) => n.id !== id), edges: flow.edges.filter((e) => e.from !== id && e.to !== id) };
}

export function change(flow: Flow, id: string, patch: Record<string, unknown>): Flow {
  const nodes = flow.nodes.map((n) => {
    if (n.id !== id) return n;
    const next: FlowNode = { ...n, ...patch };
    for (const [k, v] of Object.entries(patch)) if (v === undefined || v === "") delete next[k];
    return next;
  });
  // A case that went away takes its wires with it.
  const node = nodes.find((n) => n.id === id);
  const ports = node ? portsOf(node) : [];
  const edges = node?.kind === "switch" ? flow.edges.filter((e) => e.from !== id || ports.includes(e.port ?? "out")) : flow.edges;
  return { ...flow, nodes, edges };
}

/** A new step, somewhere it does not sit on another. */
export function add(flow: Flow, kind: string, near?: FlowNode): { flow: Flow; id: string } {
  const k = kindOf(kind);
  let n = 1;
  while (flow.nodes.some((x) => x.id === `${kind}-${n}`)) n++;
  const id = `${kind}-${n}`;
  const start = near ? { x: near.x + COLUMN, y: near.y } : { x: k.group === "Listen" ? 32 : k.group === "Decide" ? 32 + COLUMN : 32 + 2 * COLUMN, y: 32 };
  let { x, y } = start;
  while (flow.nodes.some((o) => Math.abs(o.x - x) < NODE_W && Math.abs(o.y - y) < NODE_H + 8)) y += ROW;
  return { flow: { ...flow, nodes: [...flow.nodes, { id, kind, x, y, ...k.defaults }] }, id };
}

/** Puts each step a column after whatever feeds it. */
export function arrange(flow: Flow): Flow {
  const column = new Map(flow.nodes.map((n) => [n.id, 0]));
  // A flow has no cycles, so a pass per node settles it. One drawn with a cycle is only left untidy.
  for (let i = 0; i < flow.nodes.length; i++) {
    for (const e of flow.edges) column.set(e.to, Math.max(column.get(e.to) ?? 0, (column.get(e.from) ?? 0) + 1));
  }
  const rows = new Map<number, number>();
  const nodes = flow.nodes.map((n) => {
    const c = column.get(n.id) ?? 0;
    const r = rows.get(c) ?? 0;
    rows.set(c, r + 1);
    return { ...n, x: 32 + c * COLUMN, y: 32 + r * ROW };
  });
  return { ...flow, nodes };
}

export function parseFlow(text: string | null | undefined): Flow {
  try {
    const v = JSON.parse(text ?? "") as Partial<Flow>;
    if (Array.isArray(v.nodes)) return { nodes: v.nodes, edges: Array.isArray(v.edges) ? v.edges : [], ...(v.profile === "critical" ? { profile: "critical" as const } : {}) };
  } catch {
    // A draft may be anything. What cannot be drawn starts again.
  }
  return { nodes: [], edges: [] };
}

const text = (v: unknown) => (typeof v === "string" ? v : JSON.stringify(v));

const WORDS: Record<string, string> = { ">": "above", get ">="() { return t("at least"); }, "<": "below", get "<="() { return t("at most"); }, "==": "is", get "!="() { return t("is not"); } };

/** One line on a step, saying what it has been set to. */
export function summary(n: FlowNode): string {
  const who = n.device ? t(" on {value}", { value: text(n.device) }) : "";
  switch (n.kind) {
    case "metric":
      return `${text(n.metric) || "Say which value"}${who}`;
    case "state":
      return n.device ? text(n.device) : t("Any device");
    case "event":
      return `${text(n.name) || "Say which event"}${who}`;
    case "attribute":
      return `${text(n.key) || "Say which attribute"}${who}`;
    case "timer":
    case "limit":
      return t("Every {value}", { value: text(n.every) });
    case "compare":
      return `${WORDS[text(n.op)] ?? text(n.op)} ${text(n.value)}`;
    case "range":
      return t("{value} to {value2}", { value: text(n.min), value2: text(n.max) });
    case "hold":
    case "silence":
    case "delay":
      return t("For {value}", { value: text(n.for) });
    case "changed":
      return n.by === undefined ? t("Any change") : t("By {value}", { value: text(n.by) });
    case "count":
      return t("{value} within {value2}", { value: text(n.of), value2: text(n.within) });
    case "window":
      return t("{value} over {value2}", { value: text(n.stat), value2: text(n.over) });
    case "math":
    case "combine":
      return n.formula ? text(n.formula) : t("Together");
    case "all":
      return n.any ? t("Any input") : t("Every input");
    case "alarm":
      return `${text(n.name) || "Name it"} (${text(n.severity)})`;
    case "notify":
      return text(n.title) || t("Say what");
    case "command":
      return text(n.operation) || t("Say which");
    case "export":
      return t("To {value}", { value: text(n.to) || t("which?") });
    case "record_event":
    case "record_metric":
      return text(n.name) || t("Name it");
    case "record_attribute":
      return text(n.key) || t("Name it");
    case "field":
      return text(n.path) || t("Say where");
    case "switch":
      return ((n.cases as string[]) ?? []).join(", ");
    default:
      return kindOf(n.kind).says;
  }
}

export interface Effect {
  effect: string;
  [key: string]: unknown;
}

/** What a simulated flow did, as a sentence. */
export function sayEffect(e: Effect): string {
  const on = e.device ? t(" on {value}", { value: text(e.device) }) : "";
  switch (e.effect) {
    case "alarm":
      return e.raised ? t("Raises “{value}”{on}", { value: text(e.name), on }) : t("Clears “{value}”{on}", { value: text(e.name), on });
    case "event":
      return t("Records {value}{on}", { value: text(e.name), on });
    case "metric":
      return t("Records {name} = {value}{on}", { name: text(e.name), value: text(e.value), on });
    case "attribute":
      return t("Sets {value} to {value2}{on}", { value: text(e.key), value2: text(e.value), on });
    case "notify":
      return t("Tells people: {value}", { value: text(e.title) });
    case "export":
      return t("Sends to {value}", { value: text(e.to) });
    case "command":
      return t("Asks for {value}{on}. A person or an envelope decides.", { value: text(e.operation), on });
    default:
      return e.effect;
  }
}

export interface ScriptLine {
  at: number;
  device: string;
  what: "metric" | "state" | "event" | "attribute";
  name: string;
  value: string;
}

/** A first script from the flow's own sources, so that simulating starts from something real. */
export function firstScript(flow: Flow): ScriptLine[] {
  const lines: ScriptLine[] = [];
  flow.nodes.forEach((n) => {
    const device = text(n.device ?? "DEVICE-1").replace("*", "1");
    if (n.kind === "metric") lines.push({ at: 0, device, what: "metric", name: text(n.metric), value: "1" });
    if (n.kind === "state") lines.push({ at: 0, device, what: "state", name: "", value: "Running" });
    if (n.kind === "event") lines.push({ at: 0, device, what: "event", name: text(n.name), value: "true" });
    if (n.kind === "attribute") lines.push({ at: 0, device, what: "attribute", name: text(n.key), value: "1" });
  });
  return lines;
}

const reading = (v: string): unknown => {
  try {
    return JSON.parse(v);
  } catch {
    return v;
  }
};

/** A script as the server reads it. */
export function scriptBody(lines: ScriptLine[]) {
  return lines.map((l) => (l.what === "state" ? { at: l.at, device: l.device, state: l.value } : { at: l.at, device: l.device, [l.what]: l.name, value: reading(l.value) }));
}

/** What to keep the clock running for after the script: long enough to see a hold or a silence fire. */
export function waitFor(flow: Flow, lines: ScriptLine[]): number {
  const secs = (v: unknown) => {
    const m = /^(\d+(?:\.\d+)?)(ms|s|m|h)?$/.exec(text(v));
    if (!m) return 0;
    return Number(m[1]) * { ms: 0.001, s: 1, m: 60, h: 3600 }[m[2] ?? "s"]!;
  };
  const longest = flow.nodes.reduce((most, n) => Math.max(most, ["hold", "silence", "delay"].includes(n.kind) ? secs(n.for) : 0, n.kind === "timer" ? secs(n.every) : 0), 0);
  const last = lines.reduce((most, l) => Math.max(most, l.at), 0);
  return Math.min(6 * 3600, Math.ceil(last + longest + 1));
}

/** Ways to begin, for someone looking at an empty canvas. */
export const STARTS: { name: string; says: string; flow: Flow }[] = [
  {
    get name() { return t("Alarm when a number stays high"); },
    get says() { return t("A number above a limit for a while raises an alarm, and coming back down clears it."); },
    flow: {
      nodes: [
        { id: "reading", kind: "metric", metric: "temp.c", x: 32, y: 64 },
        { id: "high", kind: "compare", op: ">", value: 80, x: 276, y: 64 },
        { id: "lasted", kind: "hold", for: "30s", x: 520, y: 32 },
        { id: "alarm", kind: "alarm", get name() { return t("Running hot"); }, severity: "Warning", x: 764, y: 64 },
      ],
      edges: [
        { from: "reading", to: "high" },
        { from: "high", port: "yes", to: "lasted" },
        { from: "high", port: "no", to: "lasted", input: "reset" },
        { from: "high", port: "no", to: "alarm", input: "clear" },
        { from: "lasted", to: "alarm" },
      ],
    },
  },
  {
    get name() { return t("Tell someone when a device goes quiet"); },
    get says() { return t("Nothing heard for a while tells the people who look after it."); },
    flow: {
      nodes: [
        { id: "reading", kind: "metric", metric: "temp.c", x: 32, y: 64 },
        { id: "quiet", kind: "silence", for: "5m", x: 276, y: 64 },
        { id: "tell", kind: "notify", title: "{device} has gone quiet", x: 520, y: 64 },
      ],
      edges: [
        { from: "reading", to: "quiet" },
        { from: "quiet", to: "tell" },
      ],
    },
  },
  {
    get name() { return t("Ask for a charge when the battery is low"); },
    get says() { return t("A low battery asks for a command. It waits for a person unless an envelope allows it."); },
    flow: {
      nodes: [
        { id: "battery", kind: "metric", metric: "battery.percent", x: 32, y: 64 },
        { id: "low", kind: "compare", op: "<", value: 20, x: 276, y: 64 },
        { id: "ask", kind: "command", operation: "robot.charge", every: "10m", x: 520, y: 64 },
      ],
      edges: [
        { from: "battery", to: "low" },
        { from: "low", port: "yes", to: "ask" },
      ],
    },
  },
  {
    get name() { return t("Work out power from volts and amps"); },
    get says() { return t("Two numbers from one device become a third, kept like any other."); },
    flow: {
      nodes: [
        { id: "volts", kind: "metric", metric: "volts", x: 32, y: 32 },
        { id: "amps", kind: "metric", metric: "amps", x: 32, y: 128 },
        { id: "power", kind: "combine", formula: "[v] * [a] / 1000", fresh: "30s", x: 276, y: 80 },
        { id: "keep", kind: "record_metric", name: "power.kw", unit: "kW", x: 520, y: 80 },
      ],
      edges: [
        { from: "volts", to: "power", input: "v" },
        { from: "amps", to: "power", input: "a" },
        { from: "power", to: "keep" },
      ],
    },
  },
];
