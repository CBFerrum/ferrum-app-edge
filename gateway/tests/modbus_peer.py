"""pymodbus as the far end of Modbus TCP."""

import asyncio
import json
import sys

from pymodbus.datastore import ModbusDeviceContext, ModbusSequentialDataBlock, ModbusServerContext
from pymodbus.server import StartAsyncTcpServer


async def main(port):
    # 21.5 scaled by ten, 12.5 as a float with its words swapped, and two words of a count. pymodbus
    # counts registers from one, where the protocol's addresses count from nought.
    device = ModbusDeviceContext(hr=ModbusSequentialDataBlock(1, [215, 0x0000, 0x4148, 1, 2, 0] + [0] * 200))
    print(json.dumps({"ready": True}), flush=True)
    await StartAsyncTcpServer(context=ModbusServerContext(devices={1: device}, single=False), address=("127.0.0.1", port))


asyncio.run(main(int(sys.argv[1])))
