# -*- coding: utf-8 -*-
"""All Kodi/xbmc interaction lives here (dialogs, listitems, playback).

The rest of the addon calls these; pure logic stays in engine/tmdb/util
so it can be tested without a Kodi runtime.
"""

from __future__ import annotations

from pathlib import Path

import xbmcaddon
import xbmcgui
import xbmcplugin
import xbmcvfs

from . import util
from .engine import Engine, EngineError
from .tmdb import Tmdb

ADDON_ID = "plugin.video.nzbkodi"


# -- settings / paths ----------------------------------------------------


def addon():
    return xbmcaddon.Addon(ADDON_ID)


def settings_dict() -> dict:
    """All addon settings as a plain dict for `Engine.write_config`."""
    current = addon()
    values = {}
    for key in (
        "download_dir",
        "engine_path",
        "tmdb_api_key",
        "nntp_host",
        "nntp_port",
        "nntp_ssl",
        "nntp_user",
        "nntp_password",
        "nntp_connections",
    ):
        values[key] = current.getSetting(key)
    for i in range(1, 7):
        values["indexer%d_url" % i] = current.getSetting("indexer%d_url" % i)
        values["indexer%d_key" % i] = current.getSetting("indexer%d_key" % i)
    values["nntp_port"] = current.getSettingInt("nntp_port")
    values["nntp_connections"] = current.getSettingInt("nntp_connections")
    values["nntp_ssl"] = current.getSettingBool("nntp_ssl")
    return values


def data_dir() -> Path:
    return Path(_translate_path("special://profile/addon_data/" + ADDON_ID))


def _translate_path(special: str) -> str:
    # Kodi 21: xbmc.translatePath was removed; xbmcvfs.translatePath is the
    # supported API.
    return xbmcvfs.translatePath(special)


def build_engine() -> Engine:
    """Fresh engine client with config rewritten from current settings."""
    settings = settings_dict()
    if not settings.get("download_dir"):
        raise EngineError("No download folder configured — set it in addon settings")
    if not settings.get("nntp_host"):
        raise EngineError("No Usenet server configured — set it in addon settings")
    root = data_dir()
    engine = Engine(
        settings.get("engine_path") or "nzbkodi-engine",
        root / "engine-config.json",
        root / "engine",
    )
    engine.ensure_dirs()
    engine.write_config(settings)
    return engine


def build_tmdb() -> Tmdb:
    from .tmdb import Tmdb, TmdbError

    key = addon().getSetting("tmdb_api_key")
    if not key:
        raise TmdbError("No TMDB API key configured (settings → General)")
    return Tmdb(key)


# -- dialogs / playback --------------------------------------------------


def notify(message: str, error: bool = False) -> None:
    import xbmc

    xbmc.executebuiltin(
        "Notification(nzbkodi,%s,%d,%s)"
        % (message.replace(",", "&#44;"), 6000, "DefaultIconError" if error else "DefaultIconInfo")
    )


def log(message: str) -> None:
    """Write to kodi.log at warning level (visible with default logging)."""
    import xbmc

    xbmc.log("[nzbkodi] %s" % message, xbmc.LOGWARNING)


def ok_dialog(message: str) -> None:
    xbmcgui.Dialog().ok("nzbkodi", message)


def input_dialog(prompt: str) -> str:
    return xbmcgui.Dialog().input(prompt, type=xbmcgui.INPUT_ALPHANUM) or ""


def confirm(prompt: str) -> bool:
    return xbmcgui.Dialog().yesno("nzbkodi", prompt)


def play_file(path: str) -> None:
    import xbmc

    xbmc.Player().play(path)


def play_or_browse(status: dict) -> None:
    """Play the job's video, or tell the user where it landed."""
    playable = status.get("playable_path")
    if playable:
        play_file(playable)
    else:
        final_dir = status.get("final_dir") or "the download folder"
        ok_dialog(
            "No video file found in the release.\nFiles are in:\n%s" % final_dir
        )


# -- directory listing ---------------------------------------------------


def add_item(handle: int, label: str, url: str, label2: str = "", art: dict | None = None,
             is_folder: bool = True, info: dict | None = None,
             context: list | None = None) -> None:
    item = xbmcgui.ListItem(label=label, label2=label2)
    item.setPath(url)
    if art:
        item.setArt(art)
    if info:
        item.setInfo("video", info)
    if context:
        item.addContextMenuItems(context)
    xbmcplugin.addDirectoryItem(handle, url, item, isFolder=is_folder)


def end_directory(handle: int) -> None:
    xbmcplugin.endOfDirectory(handle, succeeded=True, cacheToDisc=False)


# -- progress ------------------------------------------------------------


def progress_follow(engine: Engine, status_file: Path, title: str):
    """Foreground progress dialog; Cancel means run in background.

    Returns (status, outcome) from `Engine.wait_terminal`.
    """
    dialog = xbmcgui.DialogProgress()
    # Kodi 21: create() takes (heading, line1) only — extra lines go via update().
    dialog.create("nzbkodi — %s" % title, "Preparing download…")
    state = {"percent": 0}

    def on_update(status: dict) -> None:
        state["percent"] = max(state["percent"], _bar_percent(status))
        line1, line2 = util.stage_lines(status)
        # Kodi 21: DialogProgress.update() takes (percent, line1) only.
        text = line1 if not line2 else "%s — %s" % (line1, line2)
        dialog.update(int(state["percent"]), text)

    try:
        status, outcome = engine.wait_terminal(
            status_file, is_cancelled=dialog.iscanceled, on_update=on_update
        )
    finally:
        dialog.close()
    return status, outcome


def _bar_percent(status: dict) -> int:
    stage = status.get("stage")
    if stage == "starting":
        return 2
    if stage == "downloading":
        try:
            return max(2, min(90, round(float(status.get("percent", 0) or 0) * 0.9)))
        except (TypeError, ValueError):
            return 2
    if stage == "verifying":
        try:
            return 90 + round((float(status.get("verify_percent") or 0) / 100.0) * 9)
        except (TypeError, ValueError):
            return 90
    if stage == "extracting":
        return 99
    return 100