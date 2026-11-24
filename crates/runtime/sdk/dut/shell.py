# Runs INSIDE the Monty sandbox. Protocol helpers for a POSIX shell on the console
# (Linux boards, routers, e-readers). Each command prints one labelled line that
# the request waits for:
#
#   $ echo "kernel=$(uname -r)"
#   kernel=6.6.31+rpt-rpi-v8
#
# The echoed command line starts with `echo "`, so it never matches the reply.
from dut.core import now
from dut.flow import request, wait, _decode
from dut.signals import fail


def query(key, command, timeout=5):
    """Run `command` in the shell and return its output (first line)."""
    prefix = key + "="
    reply = request(
        f'echo "{key}=$({command})"\n',
        match=lambda f: _decode(f).startswith(prefix),
        timeout=timeout,
    )
    return _decode(reply)[len(prefix) :]


def run(command, timeout=5):
    """Run `command` in the shell and return its exit code."""
    return int(query("rc", command + " >/dev/null 2>&1; echo $?", timeout))


def assert_shell_exists(timeout=10, every=1, quiet=0.5):
    """Fail unless a shell answers on the console within `timeout` seconds.

    Nothing is typed while the device is still printing (booting): typing into a
    bootloader can stop the boot, and input during boot is lost. Once the console
    has been quiet for `quiet` seconds, the probe is repeated every `every`
    seconds until a shell answers.
    """
    deadline = now() + timeout
    while now() < deadline and wait(lambda ev: True, min(quiet, deadline - now())) is not None:
        pass
    while now() < deadline:
        try:
            if run("true", min(every, deadline - now())) == 0:
                return
        except TimeoutError:
            pass
    fail("no shell on the serial console")
