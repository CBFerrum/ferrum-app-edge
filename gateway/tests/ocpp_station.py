"""A charger, by the Python `ocpp` library, for ocpp_it.rs to meet someone else's OCPP."""
import asyncio
import sys

import websockets
from datetime import datetime, timezone
from ocpp.routing import on
from ocpp.v16 import ChargePoint as CP, call, call_result
from ocpp.v16.enums import RegistrationStatus, Action, ChargePointStatus, ChargePointErrorCode, RemoteStartStopStatus
class Station(CP):
    @on(Action.remote_start_transaction)
    async def on_start(self, id_tag, **kw):
        print("remote start", id_tag, flush=True)
        return call_result.RemoteStartTransaction(status=RemoteStartStopStatus.accepted)
async def main():
    async with websockets.connect(sys.argv[1], subprotocols=["ocpp1.6"]) as ws:
        cp = Station("CP_1", ws)
        t = asyncio.create_task(cp.start())
        r = await cp.call(call.BootNotification(charge_point_model="M1", charge_point_vendor="V1", firmware_version="1.2"))
        print("boot", r.status, r.interval, flush=True)
        r = await cp.call(call.Heartbeat()); print("heartbeat", r.current_time, flush=True)
        r = await cp.call(call.StatusNotification(connector_id=1, error_code=ChargePointErrorCode.no_error, status=ChargePointStatus.charging)); print("status ok", flush=True)
        r = await cp.call(call.Authorize(id_tag="A1")); print("authorize", r.id_tag_info, flush=True)
        r = await cp.call(call.StartTransaction(connector_id=1, id_tag="A1", meter_start=100, timestamp=datetime.now(timezone.utc).isoformat())); print("start", r.transaction_id, flush=True)
        r = await cp.call(call.MeterValues(connector_id=1, meter_value=[{"timestamp": datetime.now(timezone.utc).isoformat(), "sampled_value": [{"value": "1500", "measurand": "Energy.Active.Import.Register", "unit": "Wh"}]}])); print("meter ok", flush=True)
        await asyncio.sleep(1)
        t.cancel()
asyncio.run(main())
