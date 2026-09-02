# -*- coding: utf-8 -*-
"""Search history: per-kind recent queries, stored as JSON in the addon data
directory. Pure (no Kodi imports) so it stays unit-testable."""

from __future__ import annotations

import json
from pathlib import Path

KINDS = ("releases", "movies", "shows")
LIMIT = 20
DISPLAY_LIMIT = 5


def _path(base_dir: Path, kind: str) -> Path:
    return base_dir / ("history-%s.json" % kind)


def load(base_dir: Path, kind: str) -> list:
    """Most-recent-first queries for `kind`; empty on any read problem."""
    try:
        entries = json.loads(_path(base_dir, kind).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return []
    if not isinstance(entries, list):
        return []
    return [str(entry) for entry in entries][:LIMIT]


def record(base_dir: Path, kind: str, query: str) -> None:
    """Remember `query` as the most recent for `kind` (deduped, capped)."""
    query = query.strip()
    if not query:
        return
    entries = [e for e in load(base_dir, kind) if e.lower() != query.lower()]
    entries.insert(0, query)
    entries = entries[:LIMIT]
    base_dir.mkdir(parents=True, exist_ok=True)
    _path(base_dir, kind).write_text(
        json.dumps(entries), encoding="utf-8"
    )