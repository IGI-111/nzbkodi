# -*- coding: utf-8 -*-
import os
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))

from resources.lib import util  # noqa: E402


def test_format_size():
    assert util.format_size(0) == "0 B"
    assert util.format_size(1023) == "1023 B"
    assert util.format_size(1024) == "1.0 KB"
    assert util.format_size(1024 * 1024) == "1.0 MB"
    assert util.format_size(1_400_000_000) == "1.3 GB"  # binary GiB


def test_format_age():
    assert util.format_age(0) == "today"
    assert util.format_age(1) == "1d"
    assert util.format_age(42) == "42d"


def test_indexer_name():
    assert util.indexer_name("https://api.ninjacentral.com/api") == "ninjacentral.com"
    assert util.indexer_name("https://www.example.org/api") == "example.org"
    assert util.indexer_name("https://example.org") == "example.org"


def test_iso_datetime():
    assert util.iso_datetime(1725148800) == "2024-09-01 00:00:00"
    assert util.iso_datetime(0) == "1970-01-01 00:00:00"


def test_stage_lines_downloading():
    lines = util.stage_lines(
        {"stage": "downloading", "percent": 42.0, "speed_bps": 30_000_000,
         "bytes_done": 5, "bytes_total": 10}
    )
    assert lines[0] == "Downloading 42%"
    assert "28.6 MB/s" in lines[1]


def test_stage_lines_verifying():
    lines = util.stage_lines({"stage": "verifying", "verify_percent": 55.0})
    assert lines[0] == "Verifying PAR2 55%"


def test_stage_lines_failed():
    lines = util.stage_lines({"stage": "failed", "error": "boom"})
    assert lines == ("Failed", "boom")


def test_pid_alive_current():
    assert util.pid_alive(os.getpid()) is True
    assert util.pid_alive(0) is False


def run():
    for name, fn in sorted(globals().items()):
        if name.startswith("test_"):
            fn()
    print("test_util: OK")


if __name__ == "__main__":
    run()