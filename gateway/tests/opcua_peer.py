"""Someone else's OPC UA at either end of a gateway: asyncua as a client of its server, and as a
server that raises events and keeps history. It says what happened as lines of JSON."""

import asyncio
import json
import sys

from asyncua import Client, Server, ua


def say(**what):
    print(json.dumps(what, default=str), flush=True)


async def client(url, device, metric, user=None, password=None):
    c = Client(url)
    if user:
        c.set_user(user)
        c.set_password(password)
        await c.set_security_string("Basic256Sha256,SignAndEncrypt,client.der,client.pem")
    async with c:
        names = [(await n.read_browse_name()).Name for n in await c.nodes.objects.get_children()]
        say(devices=names)
        folder = next(n for n in await c.nodes.objects.get_children() if n.nodeid.Identifier == device)
        variable = (await folder.get_children())[0]
        say(metric=(await variable.read_browse_name()).Name, node=variable.nodeid.to_string())
        data = await variable.read_data_value()
        say(value=data.Value.Value, time=data.SourceTimestamp)
        try:
            await variable.write_value(ua.DataValue(ua.Variant(99.0, ua.VariantType.Double)))
            say(wrote=True)
        except ua.UaStatusCodeError as e:
            say(wrote=False, why=type(e).__name__)

        class Heard:
            def __init__(self):
                self.values = asyncio.Queue()

            def datachange_notification(self, node, value, data):
                self.values.put_nowait(value)

        heard = Heard()
        subscription = await c.create_subscription(100, heard)
        await subscription.subscribe_data_change(variable)
        say(watching=await asyncio.wait_for(heard.values.get(), 5))
        say(changed=await asyncio.wait_for(heard.values.get(), 10))


async def server(port):
    s = Server()
    await s.init()
    s.set_endpoint(f"opc.tcp://127.0.0.1:{port}/")
    s.set_security_policy([ua.SecurityPolicyType.NoSecurity])
    index = await s.register_namespace("urn:peer")
    press = await s.nodes.objects.add_object(ua.NodeId("Press", index), "Press")
    temperature = await press.add_variable(ua.NodeId("Temp", index), "Temp", 20.0)
    generator = await s.get_event_generator()
    async with s:
        await s.historize_node_data_change(temperature, period=None, count=100)
        for i in range(5):
            await temperature.write_value(20.0 + i)
            await asyncio.sleep(0.01)
        say(ready=temperature.nodeid.to_string())
        while True:
            generator.event.Message = ua.LocalizedText("door open")
            generator.event.Severity = 500
            await generator.trigger()
            await asyncio.sleep(0.2)


if sys.argv[1] == "client":
    asyncio.run(client(*sys.argv[2:]))
else:
    asyncio.run(server(int(sys.argv[2])))
