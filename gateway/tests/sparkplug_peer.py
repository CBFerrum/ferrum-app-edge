"""Eclipse Tahu's Sparkplug B definitions, as mqtt_spb_wrapper ships them, reading what a gateway
wrote and writing something for it to read. It takes and gives hexadecimal and JSON."""

import json
import sys

from mqtt_spb_wrapper.spb_protobuf import sparkplug_b as tahu
from mqtt_spb_wrapper.spb_protobuf import sparkplug_b_pb2 as messages


def read(text):
    payload = messages.Payload()
    payload.ParseFromString(bytes.fromhex(text))
    metrics = []
    for m in payload.metrics:
        kind = m.WhichOneof("value")
        metrics.append({"name": m.name, "datatype": m.datatype, "value": getattr(m, kind) if kind else None})
    print(json.dumps({"seq": payload.seq, "timestamp": payload.timestamp, "metrics": metrics}))


def write():
    payload = tahu.getDdataPayload()
    tahu.addMetric(payload, "temp.c", None, tahu.MetricDataType.Double, 21.5)
    tahu.addMetric(payload, "count", None, tahu.MetricDataType.Int64, 7)
    tahu.addMetric(payload, "on", None, tahu.MetricDataType.Boolean, True)
    tahu.addMetric(payload, "name", None, tahu.MetricDataType.String, "press")
    print(payload.SerializeToString().hex())


read(sys.argv[2]) if sys.argv[1] == "read" else write()
