# Text device (SPEC §6.4).
from dut import (
    Device,
    Framing,
    Text,
    request,
    write,
    wait_until,
    heartbeat,
    identity,
    Counter,
    Gauge,
    state,
)

dev = Device(
    "sensor-v2",
    sdk="0.1",
    framing=Framing.delimiter(b"\n"),
    format=Text(),
    heartbeat_within=10,
    channels={"low": 2402e6, "mid": 2440e6, "high": 2480e6},
)

temp_c = Gauge("temp_c", "Temperature", unit="°C")
errors = Counter("errors", "ERR lines from the device", labels=["code"])


@dev.on_line(r"FW (?P<fw>\S+)")
def firmware(fw):
    identity({"fw": fw})  # firmware from the boot log; reappearing = reboot


@dev.on_line(r"T=(?P<temp>-?[0-9.]+)$")
def temperature(temp):
    heartbeat()
    temp_c.set(float(temp))


@dev.on_line(r"\[radio\] TX started")
def radio_started():
    state["tx"] = True


@dev.on_line(r"ERR (?P<code>\S+)")
def error(code):
    heartbeat(ok=False)
    errors.inc(code=code)


@dev.every(seconds=5)  # only for devices that need polling
def poll():
    write("status\n")


@dev.set_channel
def set_channel(channel: int):
    request(f"radio ch {channel}\n", match="ch=", timeout=1)


@dev.tx_on
def tx_on():
    write("radio tx on\n")
    wait_until(lambda: state.get("tx") == True, timeout=5)


@dev.hook("recover")
def recover():
    write("reboot\n")
