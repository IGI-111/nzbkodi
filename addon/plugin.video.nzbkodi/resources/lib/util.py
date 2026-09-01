# -*- coding: utf-8 -*-
"""Small pure helpers shared across the addon (no Kodi imports)."""

from __future__ import annotations

import time
from urllib.parse import urlparse


def format_size(num_bytes: int | float) -> str:
    """Human-readable byte size: `1.4 GB`, `850 MB`, `12 KB`."""
    size = float(num_bytes or 0)
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if size < 1024.0 or unit == "TB":
            if unit == "B":
                return "%d %s" % (size, unit)
            return "%.1f %s" % (size, unit)
        size /= 1024.0
    return "%.1f TB" % size


def format_age(age_days: int) -> str:
    if age_days <= 0:
        return "today"
    if age_days == 1:
        return "1d"
    return "%dd" % age_days


def iso_datetime(unix: int) -> str:
    """Unix timestamp as Kodi's `dateadded` format."""
    import time

    return time.strftime("%Y-%m-%d %H:%M:%S", time.gmtime(unix or 0))


def indexer_name(url: str) -> str:
    """A friendly display name derived from the indexer's API URL."""
    try:
        host = urlparse(url).hostname or url
    except ValueError:
        host = url
    host = host.lower()
    for prefix in ("www.", "api.", "usenet."):
        if host.startswith(prefix):
            host = host[len(prefix):]
    return host or url


def speed_text(bps: int | float) -> str:
    if not bps:
        return "0 B/s"
    return "%s/s" % format_size(bps)


def stage_lines(status: dict) -> tuple[str, str]:
    """Map engine status to (line1, line2) for dialogs and listings.

    Pure: takes the parsed status JSON, returns display text.
    """
    stage = status.get("stage", "starting")
    if stage == "downloading":
        speed = speed_text(status.get("speed_bps", 0))
        line1 = "Downloading %s" % (
            _percent_text(status.get("percent")),
        )
        done = format_size(status.get("bytes_done", 0))
        total = format_size(status.get("bytes_total", 0))
        return line1, "%s / %s — %s" % (done, total, speed)
    if stage == "verifying":
        return "Verifying PAR2 %s" % _percent_text(
            status.get("verify_percent") or 0
        ), ""
    if stage == "extracting":
        return "Unpacking archives", ""
    if stage == "starting":
        return "Preparing download", ""
    if stage == "done":
        return "Ready to play", ""
    if stage == "failed":
        return "Failed", status.get("error") or "unknown error"
    if stage == "cancelled":
        return "Cancelled — resumable", ""
    return stage, ""


def _percent_text(percent) -> str:
    try:
        return "%d%%" % round(float(percent))
    except (TypeError, ValueError):
        return "0%"


def now_unix() -> int:
    return int(time.time())


def pid_alive(pid: int) -> bool:
    """True if a process with this pid exists (signal-0 probe)."""
    if not pid or pid <= 0:
        return False
    try:
        import os

        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True