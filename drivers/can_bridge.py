#!/usr/bin/env python3
"""CAN frames as readings, for a gateway with a CAN interface: an external driver, because the
interfaces differ and python-can knows them all. Each frame is an event named after its id, and a
command `can.send` with {"id": 291, "data": "01ff"} sends one. Put it in the site's drivers_dir, and
`connect: process://can_bridge.py socketcan can0`.

Needs `pip install python-can`. Nothing here has met a CAN bus: it has run on python-can's virtual one.
"""
import json
import sys
import threading

import can


def say(line):
    sys.stdout.write(json.dumps(line) + "\n")
    sys.stdout.flush()


def listen(bus, device):
    for frame in bus:
        say({"event": f"can.{frame.arbitration_id:x}", "value": frame.data.hex(), "device": device})


def main():
    interface, channel = sys.argv[1], sys.argv[2]
    device = sys.argv[3] if len(sys.argv) > 3 else None
    bus = can.Bus(interface=interface, channel=channel)
    threading.Thread(target=listen, args=(bus, device), daemon=True).start()
    for line in sys.stdin:
        asked = json.loads(line)
        if asked.get("op") != "dispatch":
            continue
        try:
            payload = asked.get("payload") or {}
            bus.send(can.Message(arbitration_id=int(payload["id"]), data=bytes.fromhex(payload["data"]), is_extended_id=False))
            say({"command": asked["command"], "status": "done"})
        except Exception as why:
            say({"command": asked["command"], "status": "failed", "why": str(why)})


if __name__ == "__main__":
    main()
