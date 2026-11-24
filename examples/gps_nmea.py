# GNSS receiver speaking NMEA 0183 (u-blox, Quectel, MediaTek, ...), 9600 or 38400 8N1.
#
#   dut run examples/gps_nmea.py --port /dev/ttyUSB0 --baud 38400 --verb status
#
# Without hardware, on a real u-blox NEO-M9N capture from the gpsd project:
#
#   dut replay examples/gps_nmea.py examples/logs/ublox-neo-m9n-nmea.log
from dut import Device, Framing, heartbeat, state, wait_until, Counter, Gauge


def checksum(body):
    value = 0
    for c in body:
        value = value ^ ord(c)
    return value


class Nmea:
    """`$<body>*<hh>`: the checksum is the XOR of the body's characters."""

    def decode(self, frame):
        line = frame.decode("ascii")
        if not line.startswith("$") or len(line) < 4 or line[-3] != "*":
            raise ValueError("not an NMEA sentence")
        body = line[1:-3]
        if checksum(body) != int(line[-2:], 16):
            raise ValueError("bad checksum")
        return body

    def encode(self, sentence):
        return f"${sentence}*{checksum(sentence):02X}\r\n".encode()


dev = Device(
    "gnss-nmea",
    sdk="0.1",
    framing=Framing.delimiter(b"\r\n"),
    format=Nmea(),
    heartbeat_within=5,
)

fix_quality = Gauge("fix_quality", "0 none, 1 GPS, 2 DGPS, 4 RTK fixed, 5 RTK float")
satellites = Gauge("satellites", "Satellites used in the fix")
hdop = Gauge("hdop", "Horizontal dilution of precision")
altitude = Gauge("altitude_m", "Altitude above mean sea level", unit="m")
speed = Gauge("speed_kn", "Speed over ground", unit="kn")
fixes = Counter("fixes", "Position fixes reported")


# $GNGGA,223745.00,3806.62964,N,12237.61382,W,2,12,0.54,83.1,M,-29.5,M,,0000
@dev.on_line(r"..GGA,(?:[^,]*,){5}(?P<quality>\d),(?P<sats>\d+),(?P<dop>[\d.]+),(?P<alt>-?[\d.]+),M")
def gga(quality, sats, dop, alt):
    state["fix"] = int(quality)
    heartbeat(ok=int(quality) > 0)
    fix_quality.set(int(quality))
    satellites.set(int(sats))
    hdop.set(float(dop))
    altitude.set(float(alt))
    if int(quality) > 0:
        fixes.inc()


# $GNRMC,223745.00,A,3806.62964,N,12237.61382,W,0.040,,110720,,,D,V
@dev.on_line(r"..RMC,[^,]*,A,(?:[^,]*,){4}(?P<knots>[\d.]+),")
def rmc(knots):
    speed.set(float(knots))


@dev.status
def status():
    # healthy: valid sentences with a position fix within a few seconds
    wait_until(lambda: state.get("fix", 0) > 0, timeout=5)
    return True
