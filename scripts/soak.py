#!/usr/bin/env python3
"""How much one gateway carries, end to end: a device publishing over MQTT, through the gateway's
journal, to rows in PostgreSQL. Run it against a plant of your own and a release build: every number
depends on the disk, the network and the build, and a debug build says nothing.

    soak.py HOST PORT TOKENS --rate 3000 --metrics 10 --seconds 20 --db postgres://...

Each token is one connection to the gateway's device API, publishing `rate` messages a second, each
holding `metrics` values. It needs paho-mqtt 2, and psql for the count. `/metrics` on the console says
what was dropped and shed. The counts include whatever else the plant is reading meanwhile."""

import argparse
import json
import subprocess
import threading
import time

import paho.mqtt.client as mqtt

ap = argparse.ArgumentParser()
ap.add_argument("host")
ap.add_argument("port", type=int)
ap.add_argument("tokens", help="comma separated")
ap.add_argument("--rate", type=int, default=100)
ap.add_argument("--metrics", type=int, default=3)
ap.add_argument("--seconds", type=int, default=30)
ap.add_argument("--db", required=True)
args = ap.parse_args()
tokens = args.tokens.split(",")


def stored():
    sql = "select set_config('ferrum.system', 'on', false); select count(*) from op_measurement"
    out = subprocess.run(["psql", args.db, "-Atc", sql], capture_output=True, text=True)
    return int(out.stdout.split()[-1])


sent = [0] * len(tokens)
stop = threading.Event()


def device(i, token):
    c = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2, client_id=f"soak-{i}", protocol=mqtt.MQTTv311)
    c.username_pw_set(token)
    c.connect(args.host, args.port, 60)
    c.loop_start()
    gap = 1 / args.rate
    next_at = time.monotonic()
    n = 0
    while not stop.is_set():
        n += 1
        body = {f"m{k}": (n + k) % 1000 / 10 for k in range(args.metrics)}
        c.publish("v1/devices/me/telemetry", json.dumps(body), qos=0)
        sent[i] += 1
        next_at += gap
        delay = next_at - time.monotonic()
        if delay > 0:
            time.sleep(delay)
    c.loop_stop()
    c.disconnect()


before = stored()
threads = [threading.Thread(target=device, args=(i, t)) for i, t in enumerate(tokens)]
started = time.monotonic()
[t.start() for t in threads]
time.sleep(args.seconds)
stop.set()
[t.join() for t in threads]
elapsed = time.monotonic() - started
# The rest of the plant goes on reading, so settled means hardly anything more arrived.
during, settled = stored() - before, time.monotonic() - started
for _ in range(120):
    time.sleep(1)
    now = stored() - before
    if now - during < 20:
        break
    during, settled = now, time.monotonic() - started
values = sum(sent) * args.metrics
print(json.dumps({
    "devices": len(tokens),
    "messages_sent": sum(sent),
    "values_sent": values,
    "sent_per_second": round(values / elapsed),
    "values_stored": during,
    "stored_per_second_overall": round(during / settled),
    "seconds_to_settle": round(settled, 1),
    "lost": values - during,
}))
