# Binary device, COBS framing + CRC16 trailer (SPEC §6.6).
# The [op, len, payload] header and the opcodes are this device's protocol.
from dut import (
    Device,
    Framing,
    request,
    heartbeat,
    record,
    event,
    pack,
    unpack,
    cobs_encode,
    with_crc,
    check_crc,
)

dev = Device(
    "meter-x",
    sdk="0.1",
    framing=Framing.cobs(),
    heartbeat_within=5,
    events={"crc_error": "A frame from the meter failed its CRC check"},
)


def cmd(op, payload=b""):
    return cobs_encode(with_crc(pack("<BB", [op, len(payload)]) + payload))


@dev.on_frame
def parse(frame):
    body = check_crc(frame)
    if body is None:
        event("crc_error")
        return
    if body[0] == 0x10:
        heartbeat()
        temp, vbat = unpack("<hH", body[2:6])
        record({"temp_c": temp / 100, "vbat_mv": vbat})


@dev.tx_on
def tx_on():
    request(cmd(0x21, pack("<B", [1])), match=lambda f: f[0] == 0x21, timeout=1)
