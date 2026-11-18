import pickle
import sys
import time

import pytest
from conftest import text_page

import sealwax


def sleep(seconds: float) -> str:
    time.sleep(seconds)
    return "slept"


def allocate(size: int) -> int:
    return len(bytearray(size))


def test_limits_pickle():
    limits = sealwax.Limits(timeout=30, max_operators=3, min_dash_period=0.5, max_unpacked_bytes=1_000)
    assert pickle.loads(pickle.dumps(limits)) == limits
    assert pickle.loads(pickle.dumps(sealwax.Limits.unbounded())) == sealwax.Limits.unbounded()


def test_the_result_and_the_exception_come_back():
    converted, report = sealwax.isolated(sealwax.pdfa_convert, text_page(), level="3b")
    assert converted.startswith(b"%PDF") and report["residual"] == {}
    with pytest.raises(sealwax.PdfLimitError) as raised:
        sealwax.isolated(sealwax.pdfa_convert, text_page(b"q q q Q Q Q"), limits=sealwax.Limits(max_operators=3))
    assert raised.value.limit == "max_operators"
    with pytest.raises(sealwax.PdfReadError):
        sealwax.isolated(sealwax.read_geometry, b"not a pdf", 10)


def test_past_the_timeout_the_child_is_killed():
    started = time.monotonic()
    with pytest.raises(sealwax.PdfTimeoutError) as raised:
        sealwax.isolated(sleep, 30, timeout=0.5)
    assert raised.value.limit == "deadline"
    assert time.monotonic() - started < 10
    assert sealwax.isolated(sleep, 0, timeout=10) == "slept"


@pytest.mark.skipif(sys.platform != "linux", reason="the address space is bounded on Linux only")
def test_past_the_memory_the_child_dies_alone():
    with pytest.raises((sealwax.PdfLimitError, MemoryError)):
        sealwax.isolated(allocate, 4 << 30, memory=1 << 30)
