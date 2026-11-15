# Text device (SPEC §6.4).
from dut import (
    Device,
    Framing,
    request,
    write,
    wait_until,
    heartbeat,
    identity,
    record,
    event,
    state,
)

dev = Device(
    "sensor-v2",
    sdk="0.1",
    framing=Framing.delimiter(b"\n"),
    heartbeat_within=10,
    channels={"low": 2402e6, "mid": 2440e6, "high": 2480e6},
    events={
        "error": {
            "description": "The device printed an ERR line",
            "fields": {"text": "the line as printed"},
        }
    },
)


@dev.on_frame
def parse(frame):
    line = frame.decode()
    if line.startswith("FW "):
        identity({"fw": line[3:]})  # firmware from boot log; reappearing = reboot
    elif line.startswith("T="):
        heartbeat()
        record({"temp_c": float(line[2:])})
    elif line.startswith("[radio] TX started"):
        state["tx"] = True
    elif line.startswith("ERR"):
        heartbeat(ok=False)
        event("error", text=line)


@dev.every(seconds=5)  # only for devices that need polling
def poll():
    write(b"status\n")


@dev.set_channel
def set_channel(channel: int):
    request(
        b"radio ch " + str(channel).encode() + b"\n",
        match=lambda f: f.startswith(b"ch="),
        timeout=1,
    )


@dev.tx_on
def tx_on():
    write(b"radio tx on\n")
    wait_until(lambda: state.get("tx") == True, timeout=5)


@dev.hook("recover")
def recover():
    write(b"reboot\n")
