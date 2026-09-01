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

API_ENDPOINTS = [
    "https://api.themoviedb.org/3",
    "https://api.tmdb.org/3",  # Elementum's fallback: some ISPs black-hole the other host
]
IMAGE_BASE = "https://image.tmdb.org/t/p/w500"
REQUEST_TIMEOUT = 10
# getaddrinfo() has no timeout of its own; a black-holed DNS entry hangs
# here for 30s+ (and Kodi kills the plugin at 30s). Bound it with a thread.
RESOLVE_TIMEOUT = 5.0

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


def resolve_bounded(host: str, timeout: float = RESOLVE_TIMEOUT) -> list:
    """getaddrinfo with a wall-clock bound, IPv4 addresses first.

    Python's getaddrinfo has no timeout (a black-holed name can hang past
    Kodi's 30s plugin kill), and Python has no Happy Eyeballs: urllib
    iterates every AAAA record with the timeout applied per address,
    which turns one flaky IPv6 route into a multi-minute hang. Here the
    lookup runs in a worker thread (abandoned past `timeout`), and the
    returned list is ordered IPv4-first so a working v4 route is tried
    before any v6 address.
    """
    import socket
    import threading

    result = {}

    def worker():
        try:
            result["infos"] = socket.getaddrinfo(
                host, 443, 0, socket.SOCK_STREAM
            )
        except OSError as exc:
            result["error"] = exc

    thread = threading.Thread(target=worker, daemon=True)
    thread.start()
    thread.join(timeout)
    if thread.is_alive():
        raise TmdbError("DNS lookup timed out for %s" % host)
    if "error" in result:
        raise TmdbError("DNS lookup failed for %s: %s" % (host, result["error"]))
    infos = result.get("infos") or []
    return ([i for i in infos if i[0] == socket.AF_INET]
            + [i for i in infos if i[0] == socket.AF_INET6])


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
        self._resolver = resolve_bounded

    @property
    def api_key(self) -> str:
        return self._keys[self._key_index]

    def _connect(self, host: str):
        """Open an HTTPS connection, IPv4-first, per-address timeout."""
        import http.client
        import socket
        import ssl

        infos = self._resolver(host)
        last_error = None
        for af, socktype, proto, _canon, sockaddr in infos:
            sock = socket.socket(af, socktype, proto)
            sock.settimeout(REQUEST_TIMEOUT)
            try:
                sock.connect(sockaddr)
            except OSError as exc:
                last_error = exc
                sock.close()
                continue
            conn = http.client.HTTPSConnection(host, timeout=REQUEST_TIMEOUT)
            conn.sock = conn._context.wrap_socket(
                sock, server_hostname=host
            )
            return conn
        raise TmdbError("could not connect to %s: %s" % (host, last_error))

    def _get(self, path: str, **params) -> dict:
        query = {k: v for k, v in params.items() if v is not None}
        errors = []
        for endpoint in API_ENDPOINTS:
            host = urllib.parse.urlparse(endpoint).hostname or ""
            # Endpoints carry a path prefix ("/3") that must reach the
            # request line, not just the connection host.
            base_path = urllib.parse.urlparse(endpoint).path.rstrip("/")
            try:
                # Key-rotation loop: retry auth rejections with next key.
                while True:
                    full = dict(query)
                    full["api_key"] = self.api_key
                    conn = self._connect(host)
                    try:
                        conn.request(
                            "GET", "%s%s?%s" % (base_path, path, urllib.parse.urlencode(full))
                        )
                        response = conn.getresponse()
                        body = response.read().decode("utf-8")
                        if response.status in (401, 403):
                            if self._key_index + 1 < len(self._keys):
                                self._key_index += 1
                                continue
                            raise TmdbError(
                                "TMDB rejected the API key (HTTP %d)" % response.status
                            )
                        if response.status != 200:
                            raise TmdbError("TMDB HTTP %d for %s" % (response.status, path))
                        return json.loads(body)
                    finally:
                        conn.close()
            except (TmdbError, OSError, ValueError) as exc:
                errors.append("%s: %s" % (host, exc))
                continue  # next endpoint
        raise TmdbError("TMDB request failed: %s" % "; ".join(errors))

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