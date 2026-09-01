# -*- coding: utf-8 -*-
"""URL routing for the plugin entry point."""

from __future__ import annotations

import sys
from urllib.parse import parse_qs, urlparse

from . import browsing, kodiui, picking
from .engine import EngineError
from .tmdb import TmdbError


def main(argv: list) -> None:
    handle = int(argv[1]) if len(argv) > 1 else 0
    try:
        route(handle, parse_params(argv))
    except (EngineError, TmdbError) as exc:
        kodiui.notify(str(exc), error=True)
        kodiui.end_directory(handle)


def parse_params(argv: list) -> dict:
    """Plugin URL parameters, wherever Kodi puts them.

    Kodi invokes pluginsource addons as argv = [base_url, handle, "?query"] —
    the query string arrives in argv[2], stripped from argv[0]. Direct
    invocations (tests, kodi-send) may embed it in argv[0] instead.
    """
    query = ""
    if len(argv) > 2 and argv[2]:
        query = argv[2]
    elif argv and "?" in argv[0]:
        query = urlparse(argv[0]).query
    if query.startswith("?"):
        query = query[1:]
    return {
        key: values[0]
        for key, values in parse_qs(query).items()
    }


def route(handle: int, params: dict) -> None:
    action = params.get("action", "")
    if action in ("", "root"):
        browsing.show_root(handle)
    elif action == "search":
        browsing.do_search(handle, params.get("query"))
    elif action == "movies":
        browsing.show_movies(handle)
    elif action == "movies_search":
        browsing.do_movies_search(handle, params.get("query"))
    elif action == "shows":
        browsing.show_shows(handle)
    elif action == "shows_search":
        browsing.do_shows_search(handle, params.get("query"))
    elif action == "show":
        browsing.show_show_seasons(
            handle, int(params["tmdb"]), params.get("title", "")
        )
    elif action == "episodes":
        browsing.show_season_episodes(
            handle,
            int(params["tmdb"]),
            int(params["season"]),
            params.get("title", ""),
        )
    elif action == "releases":
        browsing.show_releases(
            handle,
            kind=params.get("kind", "text"),
            title=params.get("title", ""),
            query=params.get("query"),
            season=params.get("season"),
            episode=params.get("episode"),
            tmdb=params.get("tmdb"),
            poster=params.get("poster"),
        )
    elif action == "pick":
        picking.pick_release(
            params.get("nzb", ""),
            params.get("title", ""),
            params.get("release", ""),
        )
    elif action == "downloads":
        browsing.show_downloads(handle)
    elif action == "open_download":
        picking.open_download(params["file"])
    elif action == "cancel":
        picking.cancel_download(params["file"])
    elif action == "retry":
        picking.retry_download(params["file"])
    elif action == "forget":
        picking.forget_download(params["file"])
    else:
        kodiui.notify("Unknown action: %s" % action, error=True)
        browsing.show_root(handle)


if __name__ == "__main__":
    main(sys.argv)