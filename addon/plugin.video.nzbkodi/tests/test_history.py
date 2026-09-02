# -*- coding: utf-8 -*-
import os
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))

import tempfile  # noqa: E402
from pathlib import Path  # noqa: E402

from resources.lib import history  # noqa: E402


def test_roundtrip_and_order():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        history.record(base, "shows", "mr robot")
        history.record(base, "shows", "severance")
        history.record(base, "shows", "patton")
        assert history.load(base, "shows") == ["patton", "severance", "mr robot"]


def test_dedupe_moves_to_top():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        history.record(base, "shows", "mr robot")
        history.record(base, "shows", "severance")
        history.record(base, "shows", "MR ROBOT")
        assert history.load(base, "shows") == ["MR ROBOT", "severance"]


def test_cap():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        for i in range(30):
            history.record(base, "movies", "q%02d" % i)
        entries = history.load(base, "movies")
        assert len(entries) == history.LIMIT
        assert entries[0] == "q29"


def test_kinds_are_isolated():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        history.record(base, "movies", "dune")
        history.record(base, "shows", "dune")
        assert history.load(base, "movies") == ["dune"]
        history.record(base, "shows", "severance")
        assert history.load(base, "movies") == ["dune"]
        assert history.load(base, "shows") == ["severance", "dune"]


def test_empty_query_is_ignored():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        history.record(base, "shows", "   ")
        history.record(base, "shows", "")
        assert history.load(base, "shows") == []


def test_corrupt_file_reads_empty():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        (base / "history-shows.json").write_text("{not json")
        assert history.load(base, "shows") == []


def test_missing_file_reads_empty():
    with tempfile.TemporaryDirectory() as tmp:
        assert history.load(Path(tmp), "shows") == []


def run():
    for name, fn in sorted(globals().items()):
        if name.startswith("test_"):
            fn()
    print("test_history: OK")


if __name__ == "__main__":
    run()