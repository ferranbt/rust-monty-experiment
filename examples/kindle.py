# Amazon Kindle on its internal serial console (UART test pads, 115200 8N1).
#
#   dut run examples/kindle.py --port /dev/ttyUSB0 --verb status --duration 60
#
# Needs a root shell on the console (usually a jailbroken device) and an adapter at
# the right logic level: 1.8 V on most recent models, 3.3 V on some older ones.
# The firmware is Linux + busybox; device state comes from `lipc-get-prop`, whose
# property names vary by model and firmware: check them on your device first.
#
# Like the Raspberry Pi example, each command prints one labelled line:
#
#   $ echo "battery=$(lipc-get-prop com.lab126.powerd battLevel)"
#   battery=87
from dut import Device, Framing, request, heartbeat, identity, record, event, log, fail
from dut.shell import query, run, assert_shell_exists

dev = Device(
    "kindle",
    sdk="0.1",
    framing=Framing.delimiter(b"\r\n"),
    heartbeat_within=30,
    events={
        "reboot": "The Kindle rebooted: the kernel boot banner appeared on the console",
        "battery_low": {
            "description": "Battery level dropped below 10 %",
            "fields": {"level": "battery level in %"},
        },
    },
)


@dev.on_frame
def parse(frame):
    line = frame.decode()
    log(line)

    if "Booting Linux" in line:
        event("reboot")


@dev.hook("prepare")
def prepare():
    assert_shell_exists()
    identity(
        {
            "firmware": query("firmware", "cat /etc/prettyversion.txt"),
            "serial": query("serial", "cat /proc/usid"),
        }
    )


@dev.every(seconds=10)
def telemetry():
    level = int(query("battery", "lipc-get-prop com.lab126.powerd battLevel"))
    charging = query("charging", "lipc-get-prop com.lab126.powerd isCharging") == "1"
    heartbeat()
    record({"battery_pct": level, "charging": charging})
    if level < 10:
        event("battery_low", level=level)


@dev.status
def status():
    level = int(query("battery", "lipc-get-prop com.lab126.powerd battLevel"))
    if level < 5:
        fail(f"battery critical: {level} %")
    return True


# The Wi-Fi radio, for radiated tests in an anechoic chamber.
@dev.tx_on
def tx_on():
    if run("lipc-set-prop com.lab126.wifid enable 1") != 0:
        fail("could not enable Wi-Fi")


@dev.hook("recover")
def recover():
    request(b"reboot\n", match=lambda f: b"Booting Linux" in f, timeout=120)
