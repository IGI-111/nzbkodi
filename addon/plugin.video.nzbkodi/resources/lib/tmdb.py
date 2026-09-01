# -*- coding: utf-8 -*-
"""Minimal TMDB client over stdlib (no `requests` dependency).

Only the calls the browse menus need: movie/TV search, popular lists,
movie IMDB ids, and season episode lists. Pure parsing lives in
`_parse_*` functions so it can be tested without network.
"""

from __future__ import annotations

import json
import urllib.parse
import urllib.request

API_BASE = "https://api.themoviedb.org/3"
IMAGE_BASE = "https://image.tmdb.org/t/p/w500"
REQUEST_TIMEOUT = 10

# Default keys used when the addon setting is empty (the Elementum model:
# ship a working experience out of the box; users can override in
# settings). These are the keys bundled with Elementum (elgatito/elementum,
# tmdb/tmdb.go); TMDB v3 keys identify an app rather than guard a secret.
# On 401/403 the client rotates to the next key (Elementum's fallback
# pattern — keys occasionally get region-blocked).
DEFAULT_API_KEYS = [
    "8cf43ad9c085135b9479ad5cf6bbcbda",
    "ae4bd1b6fce2a5648671bfc171d15ba4",
    "29a551a65eef108dd01b46e27eb0554a",
]



class TmdbError(Exception):
    pass


def _parse_titles(page: dict) -> list:
    """Normalize a TMDB movie/show search page into compact dicts."""
    results = []
    for item in page.get("results") or []:
        poster = item.get("poster_path")
        entry = {
            "id": item.get("id"),
            "title": item.get("title") or item.get("name") or "",
            "year": (item.get("release_date") or item.get("first_air_date") or "")[:4],
            "poster": IMAGE_BASE + poster if poster else "",
            "overview": item.get("overview") or "",
        }
        if entry["id"] and entry["title"]:
            results.append(entry)
    return results


def _parse_episodes(season: dict) -> list:
    """Normalize a TMDB season detail payload into episode dicts."""
    episodes = []
    for ep in season.get("episodes") or []:
        still = ep.get("still_path")
        entry = {
            "season": ep.get("season_number"),
            "episode": ep.get("episode_number"),
            "title": ep.get("name") or "Episode %s" % ep.get("episode_number"),
            "air_date": ep.get("air_date") or "",
            "still": IMAGE_BASE + still if still else "",
        }
        if entry["episode"]:
            episodes.append(entry)
    return episodes


class Tmdb:
    """A thin TMDB v3 client (api_key in the query string).

    A user-provided key (addon settings) is tried first; otherwise the
    bundled defaults are used. On auth rejection the next key is tried.
    """

    def __init__(self, api_key: str = ""):
        keys = []
        user = (api_key or "").strip()
        if user:
            keys.append(user)
        keys.extend(DEFAULT_API_KEYS)
        if not keys:
            raise TmdbError("no TMDB API key available")
        self._keys = keys
        self._key_index = 0

    @property
    def api_key(self) -> str:
        return self._keys[self._key_index]

    def _get(self, path: str, **params) -> dict:
        while True:
            query = {"api_key": self.api_key}
            query.update({k: v for k, v in params.items() if v is not None})
            url = "%s%s?%s" % (API_BASE, path, urllib.parse.urlencode(query))
            try:
                with urllib.request.urlopen(url, timeout=REQUEST_TIMEOUT) as response:
                    return json.loads(response.read().decode("utf-8"))
            except urllib.error.HTTPError as exc:  # type: ignore[attr-defined]
                if exc.code in (401, 403) and self._key_index + 1 < len(self._keys):
                    self._key_index += 1
                    continue
                raise TmdbError("TMDB rejected the API key (HTTP %d)" % exc.code) from exc
            except (urllib.error.URLError, OSError, ValueError) as exc:  # type: ignore[attr-defined]
                raise TmdbError("TMDB request failed: %s" % exc) from exc

    # -- movies ----------------------------------------------------------

    def search_movies(self, query: str) -> list:
        return _parse_titles(self._get("/search/movie", query=query))

    def popular_movies(self, page: int = 1) -> list:
        return _parse_titles(self._get("/movie/popular", page=page))

    def movie_imdb_id(self, tmdb_id: int) -> str:
        data = self._get("/movie/%s/external_ids" % tmdb_id)
        imdb = data.get("imdb_id")
        if not imdb:
            raise TmdbError("TMDB has no IMDB id for movie %s" % tmdb_id)
        return imdb

    # -- tv --------------------------------------------------------------

    def search_shows(self, query: str) -> list:
        return _parse_titles(self._get("/search/tv", query=query))

    def popular_shows(self, page: int = 1) -> list:
        return _parse_titles(self._get("/tv/popular", page=page))

    def show_detail(self, tmdb_id: int) -> dict:
        data = self._get("/tv/%s" % tmdb_id)
        seasons = []
        for season in data.get("seasons") or []:
            if season.get("season_number", 0) == 0:
                continue  # skip specials
            seasons.append(
                {
                    "season": season.get("season_number"),
                    "title": season.get("name") or "Season %s" % season.get("season_number"),
                    "episode_count": season.get("episode_count", 0),
                    "poster": IMAGE_BASE + season["poster_path"] if season.get("poster_path") else "",
                }
            )
        return {
            "id": data.get("id"),
            "title": data.get("name") or "",
            "seasons": seasons,
        }

    def season_episodes(self, tmdb_id: int, season: int) -> list:
        return _parse_episodes(self._get("/tv/%s/season/%s" % (tmdb_id, season)))