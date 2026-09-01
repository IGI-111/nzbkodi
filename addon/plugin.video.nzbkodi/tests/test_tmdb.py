# -*- coding: utf-8 -*-
import os
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..")))

from resources.lib import tmdb  # noqa: E402


def test_parse_titles_movies():
    page = {
        "results": [
            {"id": 693134, "title": "Dune: Part Two", "release_date": "2024-02-27",
             "poster_path": "/x.jpg", "overview": "the second one"},
            {"id": 0, "title": "", "poster_path": None},  # dropped
        ]
    }
    titles = tmdb._parse_titles(page)
    assert len(titles) == 1
    assert titles[0]["id"] == 693134
    assert titles[0]["year"] == "2024"
    assert titles[0]["poster"] == tmdb.IMAGE_BASE + "/x.jpg"
    assert titles[0]["overview"] == "the second one"


def test_parse_titles_shows():
    page = {"results": [{"id": 949, "name": "Severance", "first_air_date": "2022-02-18"}]}
    titles = tmdb._parse_titles(page)
    assert titles[0]["title"] == "Severance"
    assert titles[0]["year"] == "2022"


def test_parse_episodes():
    season = {
        "episodes": [
            {"season_number": 2, "episode_number": 4, "name": "Cold Harbor",
             "air_date": "2025-03-06", "still_path": "/s.jpg"},
            {"season_number": 2, "episode_number": 0, "name": "junk"},  # dropped
        ]
    }
    episodes = tmdb._parse_episodes(season)
    assert len(episodes) == 1
    assert episodes[0]["title"] == "Cold Harbor"
    assert episodes[0]["still"] == tmdb.IMAGE_BASE + "/s.jpg"


def test_tmdb_user_key_wins_and_defaults_used():
    assert tmdb.Tmdb("user-key").api_key == "user-key"
    assert tmdb.Tmdb("").api_key == tmdb.DEFAULT_API_KEYS[0]
    assert tmdb.Tmdb("  ").api_key == tmdb.DEFAULT_API_KEYS[0]
    assert tmdb.Tmdb("user-key")._keys[0] == "user-key"
    # bundled defaults all present
    assert len(tmdb.DEFAULT_API_KEYS) == 3


def test_tmdb_requires_any_key():
    saved = tmdb.DEFAULT_API_KEYS
    tmdb.DEFAULT_API_KEYS = []
    try:
        tmdb.Tmdb("")
        raise AssertionError("should have raised")
    except tmdb.TmdbError:
        pass
    finally:
        tmdb.DEFAULT_API_KEYS = saved


class FakeResponse:
    def __init__(self, status, body):
        self.status = status
        self._body = body

    def read(self):
        return self._body


def test_tmdb_rotates_key_on_auth_rejection():
    client = tmdb.Tmdb("bad-key")
    requests_seen = []
    responses = [FakeResponse(401, b""), FakeResponse(200, b'{"results": []}')]

    class FakeConn:
        def __init__(self, host):
            self.host = host
        def request(self, method, path):
            requests_seen.append((client.api_key, path))
        def getresponse(self):
            return responses.pop(0)
        def close(self):
            pass

    client._connect = lambda host: FakeConn(host)
    assert client._get("/search/movie", query="x") == {"results": []}
    assert requests_seen[0][0] == "bad-key"
    assert requests_seen[1][0] == tmdb.DEFAULT_API_KEYS[0]
    assert client.api_key == tmdb.DEFAULT_API_KEYS[0]


def test_tmdb_falls_back_endpoint_when_dns_hangs():
    import socket

    client = tmdb.Tmdb("")
    seen = []

    def fake_resolver(host):
        # First endpoint (themoviedb.org) black-holed; second resolves fine.
        if "themoviedb" in host:
            seen.append("DNS failed: %s" % host)
            raise tmdb.TmdbError("DNS lookup timed out for %s" % host)
        seen.append("resolved: %s" % host)
        return [(socket.AF_INET, socket.SOCK_STREAM, 6, "", ("1.2.3.4", 443))]

    class FakeConn:
        def __init__(self, host):
            self.host = host
        def request(self, method, path):
            seen.append("GET %s%s" % (self.host, path))
        def getresponse(self):
            return FakeResponse(200, b'{"results": []}')
        def close(self):
            pass

    client._resolver = fake_resolver
    # Real connect behaviour: resolve (may raise), then open the connection.
    client._connect = lambda host: (client._resolver(host) and FakeConn(host))
    assert client._get("/movie/popular") == {"results": []}
    assert any("DNS failed" in s for s in seen)
    assert seen[-1].startswith("GET api.tmdb.org")


def test_resolver_orders_ipv4_first():
    import socket
    infos = [
        (socket.AF_INET6, socket.SOCK_STREAM, 6, "", ("2600::1", 443, 0, 0)),
        (socket.AF_INET, socket.SOCK_STREAM, 6, "", ("1.2.3.4", 443)),
        (socket.AF_INET6, socket.SOCK_STREAM, 6, "", ("2600::2", 443, 0, 0)),
    ]
    saved = socket.getaddrinfo
    socket.getaddrinfo = lambda *a, **k: infos
    try:
        ordered = tmdb.resolve_bounded("example.org")
    finally:
        socket.getaddrinfo = saved
    assert ordered[0][0] == socket.AF_INET
    assert all(i[0] == socket.AF_INET6 for i in ordered[1:])
    assert len(ordered) == 3


def run():
    for name, fn in sorted(globals().items()):
        if name.startswith("test_"):
            fn()
    print("test_tmdb: OK")


if __name__ == "__main__":
    run()