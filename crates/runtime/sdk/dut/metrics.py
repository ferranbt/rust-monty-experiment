# Runs INSIDE the Monty sandbox. Metrics: every sample is a name, a float value
# and string labels; the host adds the timestamp.
#
#   undervoltage = Counter("undervoltage", "Supply voltage dropped below ~4.63 V")
#   soc_temp = Gauge("soc_temp_c", "SoC temperature", unit="°C")
#   errors = Counter("errors", "ERR lines from the device", labels=["code"])
#
#   undervoltage.inc()          # value 1
#   soc_temp.set(48.3)
#   errors.inc(code="E42")
#
# A counter reports increments (the host keeps the total), so a device reboot
# never resets it.
from dut.core import emit, _rt


def _declare(metric, kind, name, description, unit, labels):
    if not isinstance(name, str) or name == "":
        raise ValueError("metric name must be a non-empty str")
    if name in _rt["metrics"]:
        raise ValueError(f"metric '{name}' declared twice")
    if not isinstance(description, str):
        raise TypeError(f"metric '{name}': description must be a str")
    if unit is not None and not isinstance(unit, str):
        raise TypeError(f"metric '{name}': unit must be a str")
    labels = [] if labels is None else list(labels)
    for label in labels:
        if not isinstance(label, str) or label == "":
            raise ValueError(f"metric '{name}': label names must be non-empty str")
    metric.kind = kind
    metric.name = name
    metric.description = description
    metric.unit = unit
    metric.labels = labels
    _rt["metrics"][name] = metric


def _value(metric, value):
    # bool first: True/False are 1.0/0.0
    if isinstance(value, bool):
        return 1.0 if value else 0.0
    if isinstance(value, int) or isinstance(value, float):
        return float(value)
    raise TypeError(f"metric '{metric.name}': value must be a number or bool, not {type(value).__name__}")


def _label_values(metric, given):
    for key in given:
        if key not in metric.labels:
            raise ValueError(f"metric '{metric.name}' has no label '{key}'")
    for key in metric.labels:
        if key not in given:
            raise ValueError(f"metric '{metric.name}' needs label '{key}'")
        if not isinstance(given[key], str):
            raise TypeError(f"metric '{metric.name}': label '{key}' must be a str")
    return dict(given)


def _sample(metric, value, labels):
    emit("metric", {"name": metric.name, "value": value, "labels": _label_values(metric, labels)})


class Counter:
    """Counts occurrences. `inc(n=1, **labels)` reports an increment."""

    def __init__(self, name, description, unit=None, labels=None):
        _declare(self, "counter", name, description, unit, labels)

    def inc(self, n=1, **labels):
        value = _value(self, n)
        if value < 0:
            raise ValueError(f"counter '{self.name}' can only increase")
        _sample(self, value, labels)


class Gauge:
    """A measurement. `set(value, **labels)` reports the current value."""

    def __init__(self, name, description, unit=None, labels=None):
        _declare(self, "gauge", name, description, unit, labels)

    def set(self, value, **labels):
        _sample(self, _value(self, value), labels)


def _describe():
    out = {}
    for name in _rt["metrics"]:
        m = _rt["metrics"][name]
        out[name] = {"type": m.kind, "description": m.description, "unit": m.unit,
                     "labels": list(m.labels)}
    return out
