# -*- coding: utf-8 -*-
"""Starting downloads, following their progress, and the downloads menu."""

from __future__ import annotations

from pathlib import Path

from . import kodiui, util


def pick_release(nzb_url: str, title: str, release: str) -> None:
    """Start downloading a chosen release and follow it to the screen."""
    try:
        engine = kodiui.build_engine()
        status_file = engine.start(nzb_url, title)
    except Exception as exc:  # EngineError mostly
        kodiui.ok_dialog("Could not start the download:\n%s" % exc)
        return
    follow(engine, status_file, title)


def follow(engine, status_file: Path, title: str) -> None:
    status, outcome = kodiui.progress_follow(engine, status_file, title)
    if outcome == "done":
        kodiui.play_or_browse(status)
    elif outcome == "failed":
        _offer_retry(engine, status, title)
    elif outcome == "cancelled":
        kodiui.notify("Download cancelled — resume it from Downloads")
    elif outcome == "background":
        kodiui.notify("Downloading in the background — see Downloads")
    elif outcome == "stale":
        kodiui.notify("Engine died unexpectedly — see engine.log", error=True)


def _offer_retry(engine, status: dict, title: str) -> None:
    error = status.get("error") or "unknown error"
    if kodiui.confirm("Download failed:\n%s\n\nRetry?" % error):
        retry_job(engine, status, title)


def retry_job(engine, status: dict, title: str) -> None:
    """Resume (or retry) a job by its id, following it again."""
    job_id = int(status.get("job_id") or 0)
    if job_id <= 0:
        kodiui.ok_dialog("Cannot resume: no job id recorded in the status file.")
        return
    status_file = engine.resume(job_id, title)
    follow(engine, status_file, title)


# -- downloads menu actions -----------------------------------------------


def context_items(status: dict) -> list:
    """Context-menu entries for a downloads-list row."""
    file_param = status.get("_file") or ""
    stage = status.get("stage")
    items = []
    if status.get("_stale") or stage in ("failed", "cancelled"):
        items.append(("Resume", "RunPlugin(%s)" % _route("retry", file=file_param)))
    elif stage in ("starting", "downloading", "verifying", "extracting"):
        items.append(("Cancel download", "RunPlugin(%s)" % _route("cancel", file=file_param)))
    if stage in ("done", "failed", "cancelled"):
        items.append(("Forget", "RunPlugin(%s)" % _route("forget", file=file_param)))
    return items


def open_download(file: str) -> None:
    """Select action for a downloads-list row: follow, resume, or play."""
    try:
        engine = kodiui.build_engine()
        status = engine.read_status(Path(file))
        if not status:
            kodiui.notify("Status file is unreadable", error=True)
            return
        status["_file"] = file
        stage = status.get("stage")
        if status.get("_stale") or stage in ("failed", "cancelled"):
            _offer_retry(engine, status, status.get("title") or "download")
        elif stage in ("starting", "downloading", "verifying", "extracting"):
            follow(engine, Path(file), status.get("title") or "download")
        elif stage == "done":
            kodiui.play_or_browse(status)
    except Exception as exc:
        kodiui.ok_dialog("Could not open this download:\n%s" % exc)


def cancel_download(file: str) -> None:
    try:
        engine = kodiui.build_engine()
        engine.cancel(Path(file))
        kodiui.notify("Cancelling download…")
    except Exception as exc:
        kodiui.ok_dialog("Could not cancel:\n%s" % exc)


def retry_download(file: str) -> None:
    try:
        engine = kodiui.build_engine()
        status = engine.read_status(Path(file))
        if not status:
            kodiui.notify("Status file is unreadable", error=True)
            return
        retry_job(engine, status, status.get("title") or "download")
    except Exception as exc:
        kodiui.ok_dialog("Could not resume:\n%s" % exc)


def forget_download(file: str) -> None:
    if not kodiui.confirm("Remove this entry from the list?\n(Files are kept.)"):
        return
    try:
        Path(file).unlink(missing_ok=True)
        import xbmc

        xbmc.executebuiltin("Container.Refresh")
        kodiui.notify("Forgotten")
    except OSError as exc:
        kodiui.notify("Could not remove entry: %s" % exc, error=True)


def _route(action: str, file: str) -> str:
    from .browsing import route

    return route(action, file=file)