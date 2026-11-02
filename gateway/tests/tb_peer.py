"""ThingsBoard's own client library, unchanged, talking to a gateway as it would to ThingsBoard. A device
or a gateway written for ThingsBoard should need nothing but a new address. It says what happened as
lines of JSON and stays connected until it is killed."""

import hashlib
import json
import sys
import threading
import time

from tb_device_mqtt import TBDeviceMqttClient, TBPublishInfo
from tb_gateway_mqtt import TBGatewayMqttClient

OK = TBPublishInfo.TB_ERR_SUCCESS


def say(**what):
    print(json.dumps(what), flush=True)


def connected(client):
    """Connecting returns at once, and the library connects behind it."""
    for _ in range(100):
        if client.is_connected():
            return True
        time.sleep(0.1)
    return False


def device(port, token):
    client = TBDeviceMqttClient("127.0.0.1", port=port, username=token)

    def on_request(request_id, body):
        say(rpc=body)
        client.send_rpc_reply(request_id, {"done": True})

    client.set_server_side_rpc_request_handler(on_request)
    client.connect(timeout=10)
    say(connected=connected(client))
    client.subscribe_to_all_attributes(lambda attributes, *_: say(attribute=attributes))
    say(telemetry=client.send_telemetry({"temp": 21.5}).get() == OK)
    say(stamped=client.send_telemetry({"ts": 1700000000000, "values": {"old": 1}}).get() == OK)
    say(attributes=client.send_attributes({"firmware": "1.2"}).get() == OK)
    answered = threading.Event()

    def on_answer(answer, *_):
        say(asked=answer)
        answered.set()

    client.request_attributes(client_keys=["firmware"], callback=on_answer)
    answered.wait(10)


def firmware(port, token):
    client = TBDeviceMqttClient("127.0.0.1", port=port, username=token, chunk_size=4096)
    client.set_on_firmware_received_callback(
        lambda _client, data, version: say(firmware={"bytes": len(data), "version": version, "sha256": hashlib.sha256(data).hexdigest()})
    )
    client.connect(timeout=10)
    say(connected=connected(client))
    client.get_firmware_update()


def gateway(port, token):
    client = TBGatewayMqttClient("127.0.0.1", port=port, username=token)

    def on_request(_client, body):
        say(rpc=body)
        client.gw_send_rpc_reply(body["device"], body["data"]["id"], {"done": True})

    client.gw_set_server_side_rpc_request_handler(on_request)
    client.connect(timeout=10)
    say(connected=connected(client))
    client.gw_connect_device("Pump 1")
    say(telemetry=client.gw_send_telemetry("Pump 1", {"ts": 1700000000000, "values": {"flow": 3}}).get() == OK)
    say(attributes=client.gw_send_attributes("Pump 1", {"model": "P9"}).get() == OK)


{"device": device, "firmware": firmware, "gateway": gateway}[sys.argv[1]](int(sys.argv[2]), sys.argv[3])
say(ready=True)
while True:
    time.sleep(1)
