# u-blox GNSS receiver speaking its binary UBX protocol, 38400 or 115200 8N1.
#
#   dut run examples/gps_ubx.py --port /dev/ttyUSB0 --baud 38400 --verb status
#
# Without hardware, on a real u-blox NEO-M8N capture from the gpsd project:
#
#   dut replay examples/gps_ubx.py examples/logs/ublox-neo-m8n.log
from dut import Device, Framing, heartbeat, request, pack, unpack, bytes_from_list, Gauge


def fletcher(body):
    a = 0
    b = 0
    for x in body:
        a = (a + x) & 0xFF
        b = (b + a) & 0xFF
    return bytes_from_list([a, b])


class Ubx:
    """`b5 62 | class | id | length (u16 LE) | payload | ck_a ck_b`, Fletcher-8 over class..payload.

    Messages are [class, id, payload]."""

    def decode(self, frame):
        body = frame[2:-2]
        if fletcher(body) != frame[-2:]:
            raise ValueError("bad checksum")
        return [body[0], body[1], body[4:]]

    def encode(self, message):
        cls, id_, payload = message
        body = bytes_from_list([cls, id_]) + pack("<H", [len(payload)]) + payload
        return b"\xb5\x62" + body + fletcher(body)


dev = Device(
    "gnss-ubx",
    sdk="0.1",
    framing=Framing.length_prefixed(
        header=b"\xb5\x62", length_offset=4, length_size=2, trailer_len=2, endian="little"
    ),
    format=Ubx(),
    heartbeat_within=5,
)

NAV_PVT = [0x01, 0x07]

fix_type = Gauge("fix_type", "0 none, 2 2D, 3 3D, 4 GNSS + dead reckoning, 5 time only")
satellites = Gauge("satellites", "Satellites used in the fix")
altitude = Gauge("altitude_m", "Height above mean sea level", unit="m")
h_accuracy = Gauge("h_accuracy_m", "Horizontal accuracy estimate", unit="m")
speed = Gauge("speed_mps", "Ground speed", unit="m/s")
pdop = Gauge("pdop", "Position dilution of precision")


@dev.on_frame
def message(msg):
    cls, id_, payload = msg
    if [cls, id_] == NAV_PVT and len(payload) >= 92:
        fix = payload[20]
        heartbeat(ok=fix >= 2)
        fix_type.set(fix)
        satellites.set(payload[23])
        h_msl, h_acc = unpack("<iI", payload[36:44])
        altitude.set(h_msl / 1000)
        h_accuracy.set(h_acc / 1000)
        speed.set(unpack("<i", payload[60:64])[0] / 1000)
        pdop.set(unpack("<H", payload[76:78])[0] / 100)


@dev.status
def status():
    # poll NAV-PVT (an empty payload asks for one) and require a 3D fix
    reply = request(NAV_PVT + [b""], match=lambda m: [m[0], m[1]] == NAV_PVT, timeout=2)
    if reply[2][20] < 3:
        raise RuntimeError(f"no 3D fix (fix type {reply[2][20]})")
    return True
