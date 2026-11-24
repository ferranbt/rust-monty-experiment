# ESP32 running MicroPython, on UART0 (the USB-serial port of a dev board, 115200 8N1).
#
#   dut run examples/esp32.py --port /dev/cu.usbserial-0001 --verb status --duration 30
#
# The ESP32 prints its ROM boot line on every reset ("rst:0x1 (POWERON_RESET),boot:...")
# and its crash and brown-out messages on the same UART; those are counted. Commands
# go to the MicroPython prompt, each printing one labelled line the request waits for:
#
#   >>> print("heap=" + str(gc.mem_free()))
#   heap=163824
from dut import Device, Framing, Text, request, wait, write, heartbeat, identity, fail
from dut import Counter, Gauge

dev = Device(
    "esp32-micropython",
    sdk="0.1",
    framing=Framing.delimiter(b"\r\n"),
    format=Text(),
    heartbeat_within=15,
)

reboots = Counter("reboots", "ROM boot lines seen (resets)", labels=["reason"])
crashes = Counter("crashes", "Guru Meditation errors (CPU exceptions)", labels=["cause"])
brownouts = Counter("brownouts", "Brown-out detector triggered (supply dropped)")
free_heap = Gauge("free_heap", "Free MicroPython heap", unit="bytes")
uptime = Gauge("uptime_s", "Time since boot", unit="s")


# rst:0x1 (POWERON_RESET),boot:0x13 (SPI_FAST_FLASH_BOOT)
@dev.on_line(r"rst:0x[0-9a-f]+ \((?P<reason>\w+)\)")
def booted(reason):
    reboots.inc(reason=reason)


# Guru Meditation Error: Core  0 panic'ed (LoadProhibited). Exception was unhandled.
@dev.on_line(r"Guru Meditation Error: .*\((?P<cause>\w+)\)")
def crashed(cause):
    crashes.inc(cause=cause)


@dev.on_line(r".*Brownout detector was triggered")
def brownout():
    heartbeat(ok=False)
    brownouts.inc()


def query(key, expr, timeout=2):
    """Evaluate `expr` on the MicroPython prompt and return it as text."""
    prefix = key + "="
    reply = request(
        f'print("{prefix}" + str({expr}))\r',
        match=lambda line: line.startswith(prefix),
        timeout=timeout,
    )
    return reply[len(prefix) :]


@dev.hook("prepare")
def prepare():
    # let a booting board finish: it goes silent for up to ~1 s between the
    # bootloader and MicroPython, and input typed before MicroPython listens is
    # lost. Then Ctrl-C to stop any running main.py.
    while wait(lambda ev: True, 2) is not None:
        pass
    write("\x03\x03")
    query("ready", "1", timeout=5)
    identity(
        {
            "micropython": query("version", "__import__('os').uname().version"),
            "machine": query("machine", "__import__('os').uname().machine"),
            "unique_id": query("uid", "__import__('machine').unique_id().hex()"),
        }
    )


@dev.every(seconds=5)
def telemetry():
    free_heap.set(int(query("heap", "__import__('gc').mem_free()")))
    uptime.set(int(query("ms", "__import__('time').ticks_ms()")) / 1000)
    heartbeat()  # only once both readings parsed


@dev.status
def status():
    heap = int(query("heap", "__import__('gc').mem_free()"))
    if heap < 10_000:
        fail(f"low heap: {heap} bytes free")
    return True


@dev.hook("recover")
def recover():
    write("\x03")
    request("__import__('machine').reset()\r", match=lambda line: line.startswith("rst:"), timeout=10)
