#!/usr/bin/env python3
"""Bluetooth Low Energy characteristics as readings: an external driver, because a radio belongs to
the machine it is on. Each device in the arguments is `CODE=ADDRESS=UUID`, and each notification is a
metric named `ble.{short uuid}` if its first bytes are a number, or an event if not. Put it in the
site's drivers_dir, and `connect: process://ble_bridge.py PRESS-1=AA:BB:CC:DD:EE:FF=2a6e`.

Needs `pip install bleak`. Nothing here has met a Bluetooth device.
"""
import asyncio
import json
import struct
import sys

from bleak import BleakClient


def say(line):
    sys.stdout.write(json.dumps(line) + "\n")
    sys.stdout.flush()


async def watch(code, address, uuid):
    while True:
        try:
            async with BleakClient(address) as client:
                def heard(_, data):
                    if len(data) >= 2:
                        say({"metric": f"ble.{uuid}", "value": struct.unpack("<h", bytes(data[:2]))[0], "device": code})
                    else:
                        say({"event": f"ble.{uuid}", "value": data.hex(), "device": code})
                await client.start_notify(uuid, heard)
                while client.is_connected:
                    await asyncio.sleep(1)
        except Exception as why:
            print(f"{code}: {why}", file=sys.stderr, flush=True)
        await asyncio.sleep(5)


async def main():
    await asyncio.gather(*(watch(*device.split("=")) for device in sys.argv[1:]))


if __name__ == "__main__":
    asyncio.run(main())
