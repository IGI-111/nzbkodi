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


def test_tmdb_rotates_key_on_auth_rejection():
    import urllib.error, urllib.request, io
    client = tmdb.Tmdb("bad-key")
    calls = []
    class FakeResponse:
        def __init__(self, body): self._body = body
        def read(self): return self._body
        def __enter__(self): return self
        def __exit__(self, *a): return False
    def fake_urlopen(url, timeout=None):
        calls.append(url)
        if "bad-key" in url:
            raise urllib.error.HTTPError(url, 401, "unauthorized", {}, io.BytesIO(b""))
        return FakeResponse(b'{"results": []}')
    saved = urllib.request.urlopen
    urllib.request.urlopen = fake_urlopen
    try:
        result = client._get("/search/movie", query="x")
        assert result == {"results": []}
        assert len(calls) == 2, calls
        assert client.api_key == tmdb.DEFAULT_API_KEYS[0]
        assert "bad-key" not in calls[1]
    finally:
        urllib.request.urlopen = saved


def run():
    for name, fn in sorted(globals().items()):
        if name.startswith("test_"):
            fn()
    print("test_tmdb: OK")


if __name__ == "__main__":
    run()