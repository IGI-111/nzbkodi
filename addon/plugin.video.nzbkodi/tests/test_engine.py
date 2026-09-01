# -*- coding: utf-8 -*-
import json
import os
import stat
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))

from resources.lib import engine as engine_mod  # noqa: E402

FAKE_ENGINE = r'''#!/usr/bin/env python3
"""Fake nzbkodi-engine implementing the CLI/status contract for tests."""
import json, os, sys, time

def write(path, **status):
    with open(path, "w") as fh:
        json.dump(status, fh)

def arg(name, default=None):
    if name in sys.argv:
        return sys.argv[sys.argv.index(name) + 1]
    return default

cmd = sys.argv[1] if len(sys.argv) > 1 else ""
if cmd == "search":
    hits = [{
        "title": "Fake.Release.1080p",
        "nzb_url": "https://indexer/get/fake.nzb",
        "size": 1_400_000_000,
        "age_days": 3,
        "indexers": ["indexer-a"],
    }]
    print(json.dumps(hits))
elif cmd in ("start", "resume"):
    status_path = arg("--status")
    write(status_path, version=1, pid=os.getpid(), job_id=42,
          title=arg("--name", "fake"), stage="starting", updated_at=int(time.time()))
    time.sleep(0.1)
    write(status_path, version=1, pid=os.getpid(), job_id=42,
          title=arg("--name", "fake"), stage="downloading", percent=45.0,
          speed_bps=10_000_000, bytes_done=5, bytes_total=10, updated_at=int(time.time()))
    time.sleep(0.1)
    write(status_path, version=1, pid=os.getpid(), job_id=42,
          title=arg("--name", "fake"), stage="done", percent=100.0,
          playable_path="/tmp/movie.mkv", final_dir="/tmp",
          updated_at=int(time.time()))
elif cmd == "cancel":
    status_path = arg("--status")
    with open(status_path) as fh:
        status = json.load(fh)
    status["stage"] = "cancelled"
    with open(status_path, "w") as fh:
        json.dump(status, fh)
    print("cancelled")
'''


def make_engine(tmp: Path) -> engine_mod.Engine:
    fake = tmp / "fake-engine"
    fake.write_text(FAKE_ENGINE.replace("#!/usr/bin/env python3", "#!" + sys.executable))
    fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
    return engine_mod.Engine(str(fake), tmp / "engine-config.json", tmp / "engine")


def test_write_config_shape():
    with tempfile.TemporaryDirectory() as tmp:
        eng = make_engine(Path(tmp))
        eng.write_config(
            {
                "download_dir": "/downloads",
                "nntp_host": "news.example.com",
                "nntp_port": 563,
                "nntp_ssl": True,
                "nntp_user": "u",
                "nntp_password": "p",
                "nntp_connections": 8,
                "indexer1_url": "https://api.example.com/api",
                "indexer1_key": "k1",
                "indexer2_url": "https://other.com/api",
                "indexer2_key": "k2",
                "indexer3_url": "  ",  # blank → skipped
                "indexer3_key": "",
            }
        )
        config = json.loads(Path(eng.config_path).read_text())
        assert config["nntp"] == {
            "host": "news.example.com", "port": 563, "tls": True,
            "user": "u", "password": "p", "connections": 8,
        }
        assert config["download_dir"] == "/downloads"
        assert config["data_dir"] == str(eng.data_dir)
        assert [i["name"] for i in config["indexers"]] == ["example.com", "other.com"]
        assert config["indexers"][0]["api_key"] == "k1"


def test_missing_binary_raises():
    with tempfile.TemporaryDirectory() as tmp:
        eng = engine_mod.Engine("/nonexistent/nzbkodi-engine",
                                Path(tmp) / "c.json", Path(tmp) / "engine")
        try:
            eng.resolve_executable()
            raise AssertionError("should have raised")
        except engine_mod.EngineError:
            pass


def test_search_parses_hits():
    with tempfile.TemporaryDirectory() as tmp:
        eng = make_engine(Path(tmp))
        eng.write_config({"download_dir": "/d", "nntp_host": "h",
                          "indexer1_url": "https://a/api", "indexer1_key": "k"})
        hits = eng.search_text("query")
        assert hits[0]["title"] == "Fake.Release.1080p"
        assert hits[0]["nzb_url"].endswith("fake.nzb")


def test_start_follows_to_done():
    with tempfile.TemporaryDirectory() as tmp:
        eng = make_engine(Path(tmp))
        seen = []
        status_file = eng.start("https://indexer/get/fake.nzb", "Fake Movie")
        status, outcome = eng.wait_terminal(
            status_file, on_update=lambda s: seen.append(s["stage"]), interval=0.05
        )
        assert outcome == "done"
        assert status["playable_path"] == "/tmp/movie.mkv"
        assert "downloading" in seen


def test_wait_background_on_cancel_callback():
    with tempfile.TemporaryDirectory() as tmp:
        eng = make_engine(Path(tmp))
        status_file = eng.start("https://indexer/get/fake.nzb", "Fake Movie")
        calls = {"n": 0}

        def cancelled():
            calls["n"] += 1
            return calls["n"] > 2

        _, outcome = eng.wait_terminal(status_file, is_cancelled=cancelled, interval=0.05)
        assert outcome == "background"


def test_stale_detection():
    with tempfile.TemporaryDirectory() as tmp:
        eng = engine_mod.Engine("/nonexistent", Path(tmp) / "c.json", Path(tmp) / "engine")
        eng.ensure_dirs()
        status_file = eng.new_status_file("x")
        status_file.write_text(json.dumps({
            "version": 1, "pid": 999_999_999, "stage": "downloading",
            "updated_at": int(time.time()) - 120,
        }))
        status = eng.read_status(status_file)
        assert eng.is_stale(status) is True
        _, outcome = eng.wait_terminal(status_file, interval=0.05)
        assert outcome == "stale"


def test_list_downloads_and_cancel():
    with tempfile.TemporaryDirectory() as tmp:
        eng = make_engine(Path(tmp))
        eng.write_config({"download_dir": "/d", "nntp_host": "h",
                          "indexer1_url": "https://a/api", "indexer1_key": "k"})
        status_file = eng.start("https://indexer/get/fake.nzb", "Fake Movie")
        # Wait for the fake engine to write its first status.
        deadline = time.time() + 5
        while time.time() < deadline and not status_file.exists():
            time.sleep(0.05)
        # While it runs (or after it finishes) the registry sees it.
        entries = eng.list_downloads()
        assert len(entries) == 1
        assert entries[0]["title"] == "Fake Movie"
        # Cancel flips the file to a terminal stage via the fake's cancel.
        eng.cancel(status_file)
        deadline = time.time() + 2
        while time.time() < deadline:
            if eng.read_status(status_file)["stage"] == "cancelled":
                break
            time.sleep(0.05)
        assert eng.read_status(status_file)["stage"] == "cancelled"


def test_resume_spawns_new_status_file():
    with tempfile.TemporaryDirectory() as tmp:
        eng = make_engine(Path(tmp))
        status_file = eng.resume(42, "Fake Movie")
        status, outcome = eng.wait_terminal(status_file)
        assert outcome == "done"
        assert status["job_id"] == 42


def run():
    for name, fn in sorted(globals().items()):
        if name.startswith("test_"):
            fn()
    print("test_engine: OK")


if __name__ == "__main__":
    run()