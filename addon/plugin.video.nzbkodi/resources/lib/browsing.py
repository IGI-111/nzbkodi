# -*- coding: utf-8 -*-
"""The browse menus: search, movies, TV shows, releases, downloads."""

from __future__ import annotations

import urllib.parse

from . import kodiui, util
from .engine import EngineError
from .tmdb import TmdbError

# Route-building helper: everything hangs off plugin://plugin.video.nzbkodi/
def route(action: str, **params) -> str:
    query = {"action": action}
    query.update({k: v for k, v in params.items() if v not in (None, "")})
    return "plugin://%s/?%s" % (kodiui.ADDON_ID, urllib.parse.urlencode(query))


# -- root ----------------------------------------------------------------


def show_root(handle: int) -> None:
    # Search entries are non-folder items: clicking them runs the plugin
    # script-style (no listing context), which is the only safe place to
    # open a modal dialog. Kodi/skins re-fetch listing URLs (providers,
    # refreshes) and a dialog in a listing would re-open on every re-fetch.
    kodiui.add_item(handle, "Search", route("search"), is_folder=False)
    kodiui.add_item(handle, "Movies", route("movies"))
    kodiui.add_item(handle, "TV shows", route("shows"))
    kodiui.add_item(handle, "Downloads", route("downloads"))
    kodiui.end_directory(handle)


# -- text search ----------------------------------------------------------


def do_search(handle: int, query: str | None = None) -> None:
    """Bare invocation: ask (script context), then redirect to the query URL.
    With a query: render the release listing — re-fetch-safe, no dialogs."""
    if not query:
        query = kodiui.input_dialog("Search releases")
        if not query:
            return
        kodiui.container_update(route("search", query=query))
        return
    show_releases(handle, kind="text", query=query, title=query)


# -- movies ---------------------------------------------------------------


def show_movies(handle: int) -> None:
    kodiui.add_item(handle, "Search movies…", route("movies_search"), is_folder=False)
    try:
        tmdb = kodiui.build_tmdb()
        for movie in tmdb.popular_movies():
            kodiui.add_item(
                handle,
                "%s (%s)" % (movie["title"], movie["year"]) if movie["year"] else movie["title"],
                route(
                    "releases", kind="movie", tmdb=movie["id"], title=movie["title"],
                    poster=movie["poster"],
                ),
                art={"poster": movie["poster"]} if movie["poster"] else None,
                info={"plot": movie["overview"]},
            )
    except TmdbError as exc:
        kodiui.log("tmdb error: %s" % exc)
        kodiui.add_item(handle, "TMDB unavailable — %s" % exc, route("root"))
    kodiui.set_content(handle, "movies")
    kodiui.end_directory(handle)


def do_movies_search(handle: int, query: str | None = None) -> None:
    if not query:
        query = kodiui.input_dialog("Movie title")
        if not query:
            return
        kodiui.container_update(route("movies_search", query=query))
        return
    try:
        tmdb = kodiui.build_tmdb()
        for movie in tmdb.search_movies(query):
            kodiui.add_item(
                handle,
                "%s (%s)" % (movie["title"], movie["year"]) if movie["year"] else movie["title"],
                route(
                    "releases", kind="movie", tmdb=movie["id"], title=movie["title"],
                    poster=movie["poster"],
                ),
                art={"poster": movie["poster"]} if movie["poster"] else None,
                info={"plot": movie["overview"]},
            )
    except TmdbError as exc:
        kodiui.notify(str(exc), error=True)
    kodiui.set_content(handle, "movies")
    kodiui.end_directory(handle)


# -- tv -------------------------------------------------------------------


def show_shows(handle: int) -> None:
    kodiui.add_item(handle, "Search shows…", route("shows_search"), is_folder=False)
    try:
        tmdb = kodiui.build_tmdb()
        for show in tmdb.popular_shows():
            kodiui.add_item(
                handle,
                "%s (%s)" % (show["title"], show["year"]) if show["year"] else show["title"],
                route(
                    "show", tmdb=show["id"], title=show["title"], poster=show["poster"],
                ),
                art={"poster": show["poster"]} if show["poster"] else None,
                info={"plot": show["overview"]},
            )
    except TmdbError as exc:
        kodiui.log("tmdb error: %s" % exc)
        kodiui.add_item(handle, "TMDB unavailable — %s" % exc, route("root"))
    kodiui.set_content(handle, "tvshows")
    kodiui.end_directory(handle)


def do_shows_search(handle: int, query: str | None = None) -> None:
    if not query:
        query = kodiui.input_dialog("Show title")
        if not query:
            return
        kodiui.container_update(route("shows_search", query=query))
        return
    try:
        tmdb = kodiui.build_tmdb()
        for show in tmdb.search_shows(query):
            kodiui.add_item(
                handle,
                "%s (%s)" % (show["title"], show["year"]) if show["year"] else show["title"],
                route(
                    "show", tmdb=show["id"], title=show["title"], poster=show["poster"],
                ),
                art={"poster": show["poster"]} if show["poster"] else None,
                info={"plot": show["overview"]},
            )
    except TmdbError as exc:
        kodiui.notify(str(exc), error=True)
    kodiui.set_content(handle, "tvshows")
    kodiui.end_directory(handle)


def show_show_seasons(handle: int, tmdb_id: int, title: str) -> None:
    try:
        detail = kodiui.build_tmdb().show_detail(tmdb_id)
        for season in detail["seasons"]:
            kodiui.add_item(
                handle,
                "%s (%d episodes)" % (season["title"], season["episode_count"]),
                route(
                    "episodes", tmdb=tmdb_id, season=season["season"], title=title,
                    poster=season["poster"],
                ),
                art={"poster": season["poster"]} if season["poster"] else None,
            )
    except TmdbError as exc:
        kodiui.notify(str(exc), error=True)
    kodiui.set_content(handle, "seasons")
    kodiui.end_directory(handle)


def show_season_episodes(handle: int, tmdb_id: int, season: int, title: str) -> None:
    try:
        for ep in kodiui.build_tmdb().season_episodes(tmdb_id, season):
            kodiui.add_item(
                handle,
                "S%02dE%02d — %s" % (ep["season"], ep["episode"], ep["title"]),
                route(
                    "releases",
                    kind="tv",
                    query=title,
                    season=ep["season"],
                    episode=ep["episode"],
                    title="%s S%02dE%02d" % (title, ep["season"], ep["episode"]),
                    poster=ep["still"],
                ),
                label2=ep["air_date"],
                art={"poster": ep["still"]} if ep["still"] else None,
                is_folder=False,
            )
    except TmdbError as exc:
        kodiui.notify(str(exc), error=True)
    kodiui.set_content(handle, "episodes")
    kodiui.end_directory(handle)


# -- releases -------------------------------------------------------------


def show_releases(handle: int, kind: str, title: str, query: str | None = None,
                  season: int | None = None, episode: int | None = None,
                  tmdb: int | None = None, poster: str | None = None) -> None:
    """Search all indexers and list releases; picking one starts it."""

    try:
        engine = kodiui.build_engine()
        if kind == "text":
            hits = engine.search_text(query or title)
        elif kind == "tv":
            hits = engine.search_tv(query or title, int(season or 0), int(episode or 0))
        elif kind == "movie":
            imdb = kodiui.build_tmdb().movie_imdb_id(int(tmdb))
            hits = engine.search_movie(imdb)
        else:
            raise EngineError("unknown search kind %r" % kind)
    except (EngineError, TmdbError) as exc:
        kodiui.notify(str(exc), error=True)
        kodiui.end_directory(handle)
        return

    if not hits:
        detail = getattr(engine, "last_stderr", "")
        if detail:
            kodiui.log("indexer errors: %s" % detail)
            kodiui.notify("No results — indexer errors (see kodi.log)", error=True)
        else:
            kodiui.notify("No results on your indexers")

    for hit in hits:
        sources = ",".join(hit.get("indexers") or [])
        label2 = "%s · %s · %s" % (
            util.format_size(hit.get("size", 0)),
            util.format_age(int(hit.get("age_days") or 0)),
            sources,
        )
        art = {k: poster for k in ("poster", "fanart")} if poster else None
        info = {
            "title": hit.get("title") or "",
            "size": int(hit.get("size") or 0),
            "dateadded": util.iso_datetime(int(hit.get("post_date") or 0)),
            "plot": label2,
        }
        kodiui.add_item(
            handle,
            hit.get("title") or "release",
            route(
                "pick",
                nzb=hit.get("nzb_url", ""),
                title=title,
                release=hit.get("title", ""),
            ),
            label2=label2,
            art=art,
            info=info,
            is_folder=False,
        )
    # "movies" content so skins render media layouts: poster thumbs, fanart
    # backdrop, and the label2 column (size · age · indexers) in media views.
    kodiui.set_content(handle, "movies")
    kodiui.end_directory(handle)


# -- downloads ------------------------------------------------------------


def show_downloads(handle: int) -> None:
    from . import picking

    try:
        engine = kodiui.build_engine()
        entries = engine.list_downloads()
    except EngineError as exc:
        kodiui.notify(str(exc), error=True)
        kodiui.end_directory(handle)
        return

    if not entries:
        kodiui.add_item(handle, "Nothing downloaded yet", route("root"))
    for status in entries:
        line1, line2 = util.stage_lines(status)
        if status.get("_stale"):
            line1 = "Interrupted — resumable"
        kodiui.add_item(
            handle,
            "%s — %s" % (status.get("title") or "download", line1),
            route("open_download", file=status["_file"]),
            label2=line2,
            is_folder=False,
            context=picking.context_items(status),
        )
    kodiui.end_directory(handle)