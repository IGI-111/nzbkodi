# -*- coding: utf-8 -*-
"""The addon's client for the nzbkodi-engine binary.

Everything the addon does with the engine goes through here: writing the
engine config, searching indexers, spawning detached downloads, polling
status files, and the downloads registry. No Kodi imports — this module
is unit-testable with a fake engine script.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
import uuid
from pathlib import Path

from .util import now_unix

TERMINAL_STAGES = ("done", "failed", "cancelled")
LIVE_STAGES = ("starting", "downloading", "verifying", "extracting")
POLL_INTERVAL = 0.5
STALE_AFTER_SECONDS = 20


class EngineError(Exception):
    """The engine call failed (missing binary, bad config, search error)."""


class Engine:
    """Wraps the nzbkodi-engine binary.

    `engine_path` may be a bare name (resolved via PATH) or an absolute
    path. `data_dir` is the addon's private engine data directory;
    `status_dir` lives inside it and holds one JSON status file per job.
    """

    def __init__(self, engine_path: str, config_path: Path, data_dir: Path):
        self.executable = engine_path
        self.config_path = Path(config_path)
        self.data_dir = Path(data_dir)
        self.status_dir = self.data_dir / "status"
        self.log_path = self.data_dir / "engine.log"
        # stderr of the last engine invocation (per-indexer search errors land here)
        self.last_stderr = ""

    # -- paths -----------------------------------------------------------

    def resolve_executable(self) -> str:
        """Return an executable path, raising EngineError if missing."""
        resolved = shutil.which(self.executable) or (
            self.executable if os.path.isabs(self.executable) else None
        )
        if not resolved or not os.path.isfile(resolved):
            raise EngineError(
                "nzbkodi-engine binary not found (settings: engine path)"
            )
        return resolved

    def ensure_dirs(self) -> None:
        self.data_dir.mkdir(parents=True, exist_ok=True)
        self.status_dir.mkdir(parents=True, exist_ok=True)

    def new_status_file(self, title: str) -> Path:
        """A fresh, sortable status file path for a new job."""
        safe = "".join(c if c.isalnum() or c in " .-_" else "_" for c in title)
        safe = safe.strip(" ._")[:60] or "download"
        return self.status_dir / ("%d-%s.json" % (int(time.time()), uuid.uuid4().hex[:10]))

    # -- config ----------------------------------------------------------

    def write_config(self, settings: dict) -> None:
        """Translate addon settings into the engine's config JSON.

        `settings` keys: nntp_host, nntp_port, nntp_ssl, nntp_user,
        nntp_password, nntp_connections, download_dir, indexerN_url /
        indexerN_key (N in 1..INDEXER_SLOTS).
        """
        indexers = []
        for i in range(1, INDEXER_SLOTS + 1):
            url = (settings.get("indexer%d_url" % i) or "").strip()
            key = (settings.get("indexer%d_key" % i) or "").strip()
            if url and key:
                indexers.append({"name": indexer_name_from(url), "url": url, "api_key": key})
        config = {
            "nntp": {
                "host": settings.get("nntp_host", ""),
                "port": int(settings.get("nntp_port", 563)),
                "tls": bool(settings.get("nntp_ssl", True)),
                "user": settings.get("nntp_user") or None,
                "password": settings.get("nntp_password") or None,
                "connections": int(settings.get("nntp_connections", 8)),
            },
            "indexers": indexers,
            "download_dir": settings.get("download_dir", ""),
            "data_dir": str(self.data_dir),
        }
        self.ensure_dirs()
        self.config_path.parent.mkdir(parents=True, exist_ok=True)
        with open(self.config_path, "w", encoding="utf-8") as fh:
            json.dump(config, fh, indent=2)

    # -- search ----------------------------------------------------------

    def search_text(self, query: str, limit: int = 100) -> list:
        return self._search(["--query", query, "--limit", str(limit)])

    def search_movie(self, imdb_id: str, limit: int = 100) -> list:
        return self._search(["--imdb", imdb_id, "--limit", str(limit)])

    def search_tv(self, query: str, season: int, episode: int, limit: int = 100) -> list:
        return self._search(
            [
                "--query", query,
                "--season", str(season),
                "--episode", str(episode),
                "--limit", str(limit),
            ]
        )

    def _search(self, args: list) -> list:
        self.ensure_dirs()
        proc = subprocess.run(
            [self.resolve_executable(), "search", "--config", str(self.config_path)] + args,
            capture_output=True,
            text=True,
            timeout=90,
        )
        if proc.returncode != 0:
            raise EngineError(proc.stderr.strip() or "search failed")
        self.last_stderr = (proc.stderr or "").strip()
        try:
            hits = json.loads(proc.stdout)
        except json.JSONDecodeError as exc:
            raise EngineError("bad search output: %s" % exc) from exc
        return hits or []

    # -- downloads -------------------------------------------------------

    def start(self, nzb_url: str, title: str) -> Path:
        """Spawn a detached download; returns its status file path."""
        self.ensure_dirs()
        status_file = self.new_status_file(title)
        self._spawn_detached(
            [
                "start",
                "--config", str(self.config_path),
                "--nzb-url", nzb_url,
                "--status", str(status_file),
                "--name", title,
            ]
        )
        return status_file

    def resume(self, job_id: int, title: str) -> Path:
        """Re-run an existing job (article-level resume / retry)."""
        self.ensure_dirs()
        status_file = self.new_status_file(title)
        self._spawn_detached(
            [
                "resume",
                "--config", str(self.config_path),
                "--job", str(job_id),
                "--status", str(status_file),
            ]
        )
        return status_file

    def cancel(self, status_file: Path) -> None:
        """Politely stop the engine tracked by a status file."""
        try:
            subprocess.run(
                [self.resolve_executable(), "cancel", "--status", str(status_file)],
                capture_output=True,
                text=True,
                timeout=30,
            )
        except subprocess.TimeoutExpired:
            pass

    def _spawn_detached(self, args: list) -> None:
        """Run the engine outside our process group so it survives us."""
        executable = self.resolve_executable()
        subprocess.Popen(
            [executable] + args,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
            cwd=str(self.data_dir),
        )

    # -- status ----------------------------------------------------------

    @staticmethod
    def read_status(status_file: Path) -> dict | None:
        try:
            with open(status_file, encoding="utf-8") as fh:
                return json.load(fh)
        except (OSError, json.JSONDecodeError):
            return None

    def list_downloads(self) -> list:
        """All jobs known to the addon, newest first.

        Each entry is the status dict plus `_file` (status file path) and
        `_stale` (non-terminal stage but the engine pid is gone).
        """
        entries = []
        if self.status_dir.is_dir():
            for path in sorted(self.status_dir.glob("*.json")):
                status = self.read_status(path)
                if not status:
                    continue
                status["_file"] = str(path)
                status["_stale"] = self.is_stale(status)
                entries.append(status)
        entries.sort(key=lambda s: s.get("updated_at", 0), reverse=True)
        return entries

    @staticmethod
    def is_stale(status: dict) -> bool:
        if status.get("stage") in TERMINAL_STAGES:
            return False
        if status.get("stage") not in LIVE_STAGES:
            return False
        from .util import pid_alive

        if pid_alive(int(status.get("pid", 0) or 0)):
            return False
        age = now_unix() - int(status.get("updated_at", 0) or 0)
        return age > STALE_AFTER_SECONDS

    def wait_terminal(
        self,
        status_file: Path,
        is_cancelled=None,
        on_update=None,
        interval=POLL_INTERVAL,
    ):
        """Poll a status file until it settles.

        Returns (status, outcome) where outcome is one of:
        - `done` / `failed` / `cancelled`: terminal stage reached
        - `background`: `is_cancelled()` became true (dialog dismissed)
        - `stale`: engine died without writing a terminal stage
        """
        outcome = "stale"
        status = None
        while True:
            status = self.read_status(status_file)
            if status is None:
                status = {"stage": "starting", "updated_at": now_unix(), "_file": str(status_file)}
            stage = status.get("stage", "starting")
            if stage in TERMINAL_STAGES:
                outcome = stage
                break
            if self.is_stale(status):
                outcome = "stale"
                break
            if is_cancelled is not None and is_cancelled():
                outcome = "background"
                break
            if on_update is not None:
                on_update(status)
            time.sleep(interval)
        return status, outcome


def indexer_name_from(url: str) -> str:
    from .util import indexer_name

    return indexer_name(url)


INDEXER_SLOTS = 6