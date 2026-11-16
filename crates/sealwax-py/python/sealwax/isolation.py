"""An operation run in a process of its own: a document that exhausts the memory or
the time it is given kills that process only, never the worker that asked.

The limits of `sealwax.Limits` bound what sealwax spends where it can tell in advance; what they
cannot see (allocations inside the PDF interpreter, an encrypted object stream, a JPEG 2000 image)
is bounded here, by the operating system. The child is a new Python interpreter, started with
`subprocess` rather than `multiprocessing`: a worker of a task queue (Celery's, for one) is often
a daemon process, which `multiprocessing` does not let have children.
"""

import os
import pickle
import signal
import subprocess
import sys
from collections.abc import Callable
from typing import Any, TypeVar

from ._native import PdfError, PdfLimitError, PdfTimeoutError

__all__ = ["isolated"]

T = TypeVar("T")

# What the child prints on its standard error when an allocation fails (Rust's message).
_ALLOCATION_FAILED = b"memory allocation of"


def isolated(
    function: Callable[..., T],
    /,
    *args: Any,
    timeout: float | None = None,
    memory: int | None = None,
    **kwargs: Any,
) -> T:
    """`function(*args, **kwargs)`, called in a new Python process.

    `function` must be importable by its module and qualified name (`sealwax.pdfa_convert`, or a
    module-level function of the caller), and its arguments and result picklable. `timeout` is
    in seconds; `memory`, in bytes, bounds the child's address space (Linux; elsewhere it is
    ignored).

    Raises what `function` raises; PdfTimeoutError when the child runs past `timeout`;
    PdfLimitError (`limit` "memory") when it runs out of `memory`; PdfError when it dies
    otherwise.
    """
    payload = pickle.dumps(
        {
            "module": function.__module__,
            "name": function.__qualname__,
            "args": args,
            "kwargs": kwargs,
            "path": sys.path,
            "memory": memory,
        }
    )
    child = subprocess.Popen(
        [sys.executable, "-m", "sealwax._child"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={**os.environ, "PYTHONPATH": os.pathsep.join(p for p in sys.path if p)},
    )
    try:
        out, err = child.communicate(payload, timeout=timeout)
    except subprocess.TimeoutExpired:
        child.kill()
        child.communicate()
        late = PdfTimeoutError(f"isolated operation ran past {timeout} s")
        late.limit = "deadline"
        raise late from None
    if child.returncode == 0:
        try:
            outcome, value = pickle.loads(out)
        except Exception as error:
            raise PdfError("isolated operation returned nothing readable") from error
        if outcome == "error":
            raise value
        result: T = value
        return result
    if _ALLOCATION_FAILED in err or (memory is not None and child.returncode in (-signal.SIGKILL, -signal.SIGABRT)):
        exhausted = PdfLimitError("isolated operation ran out of memory")
        exhausted.limit = "memory"
        raise exhausted
    cause = f"signal {signal.Signals(-child.returncode).name}" if child.returncode < 0 else f"status {child.returncode}"
    raise PdfError(f"isolated operation died ({cause})")
