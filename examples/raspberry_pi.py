# Raspberry Pi on its serial console (GPIO UART, 115200 8N1).
#
#   dut run examples/raspberry_pi.py --port /dev/ttyUSB0 --verb status --duration 30
#
# Assumes the serial console logs in by itself (raspi-config -> System Options ->
# Auto Login, with the serial console enabled). The console is a Linux shell: each
# command prints one labelled line that the request waits for, e.g.
#
#   $ echo "temp=$(cat /sys/class/thermal/thermal_zone0/temp)"
#   temp=48312
#
# Every console line, including the echoed commands, is logged.
from dut import Device, Framing, request, heartbeat, identity, record, event, log, fail
from dut.shell import query, run, assert_shell_exists

dev = Device(
    "raspberry-pi",
    sdk="0.1",
    framing=Framing.delimiter(b"\r\n"),
    heartbeat_within=15,
    events={
        "reboot": "The Pi rebooted: the kernel boot banner appeared on the console",
        "undervoltage": {
            "description": "The supply voltage dropped below ~4.63 V",
            "fields": {"text": "the kernel message"},
        },
    },
)


@dev.on_frame
def parse(frame):
    line = frame.decode()
    log(line)

    if "Booting Linux" in line:
        event("reboot")
    elif "Under-voltage detected" in line:
        heartbeat(ok=False)
        event("undervoltage", text=line)


@dev.hook("prepare")
def prepare():
    assert_shell_exists()
    identity(
        {
            "model": query("model", "tr -d '\\0' < /proc/device-tree/model"),
            "kernel": query("kernel", "uname -r"),
            "serial": query("serial", "grep -m1 Serial /proc/cpuinfo | cut -d' ' -f2"),
        }
    )


@dev.every(seconds=5)
def telemetry():
    millideg = query("temp", "cat /sys/class/thermal/thermal_zone0/temp")
    heartbeat()
    record({"soc_temp_c": int(millideg) / 1000})


@dev.status
def status():
    # bit 0 under-voltage now, bit 1 frequency capped, bit 2 throttled
    flags = query("throttled", "vcgencmd get_throttled | cut -d= -f2")
    if int(flags, 16) & 0x7:
        fail(f"throttled: {flags}")
    return True


@dev.hook("recover")
def recover():
    request(b"sudo reboot\n", match=lambda f: b"Booting Linux" in f, timeout=60)
