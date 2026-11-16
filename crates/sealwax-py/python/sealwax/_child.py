"""The child of `sealwax.isolated`: reads the call on its standard input, bounds its own memory,
makes the call, and writes the outcome on its standard output, pickled."""

import importlib
import pickle
import resource
import sys
from typing import Any


def main() -> None:
    call = pickle.loads(sys.stdin.buffer.read())
    if call["memory"] is not None and sys.platform == "linux":
        resource.setrlimit(resource.RLIMIT_AS, (call["memory"], call["memory"]))
    sys.path[:] = call["path"]
    target: Any = importlib.import_module(call["module"])
    for part in call["name"].split("."):
        target = getattr(target, part)
    try:
        outcome = ("ok", target(*call["args"], **call["kwargs"]))
    except Exception as error:  # noqa: BLE001 — handed back to the parent as raised
        outcome = ("error", error)
    try:
        data = pickle.dumps(outcome)
    except Exception as error:  # noqa: BLE001 — an exception that does not pickle
        data = pickle.dumps(("error", RuntimeError(f"{type(error).__name__}: {error}")))
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()


if __name__ == "__main__":
    main()
