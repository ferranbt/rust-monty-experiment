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
from dut import Device, Framing, Text, request, heartbeat, identity, fail, Counter, Gauge
from dut.shell import query, run, assert_shell_exists

dev = Device(
    "kindle",
    sdk="0.1",
    framing=Framing.delimiter(b"\r\n"),
    format=Text(),
    heartbeat_within=30,
)

battery_pct = Gauge("battery_pct", "Battery level", unit="%")
charging = Gauge("charging", "1 while charging")
reboots = Counter("reboots", "Kernel boot banners seen on the console")
battery_low = Counter("battery_low", "Battery readings below 10 %")


@dev.on_line(r".*Booting Linux")
def booted():
    reboots.inc()


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
    heartbeat()
    battery_pct.set(level)
    charging.set(query("charging", "lipc-get-prop com.lab126.powerd isCharging") == "1")
    if level < 10:
        battery_low.inc()


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
    request("reboot\n", match=lambda line: "Booting Linux" in line, timeout=120)
