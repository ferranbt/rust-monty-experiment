# Runs INSIDE the Monty sandbox. Protocol helpers for a POSIX shell on the console
# (Linux boards, routers, e-readers). Each command prints one labelled line that
# the request waits for:
#
#   $ echo "kernel=$(uname -r)"
#   kernel=6.6.31+rpt-rpi-v8
#
# The echoed command line starts with `echo "`, so it never matches the reply.
from dut.flow import request
from dut.signals import fail


def query(key, command, timeout=5):
    """Run `command` in the shell and return its output (first line)."""
    prefix = key.encode() + b"="
    reply = request(
        f'echo "{key}=$({command})"\n'.encode(),
        match=lambda f: f.startswith(prefix),
        timeout=timeout,
    )
    return reply[len(prefix) :].decode()


def run(command, timeout=5):
    """Run `command` in the shell and return its exit code."""
    return int(query("rc", command + " >/dev/null 2>&1; echo $?", timeout))


def assert_shell_exists(timeout=10):
    """Fail unless a shell answers on the console (also wakes an idle console)."""
    if run("true", timeout) != 0:
        fail("no shell on the serial console")
