# Repsetarr

[![Release](https://img.shields.io/github/v/release/jshaptic/Repsetarr?label=release)](https://github.com/jshaptic/Repsetarr/releases)
[![Rust](https://img.shields.io/badge/Rust-2024-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Axum](https://img.shields.io/badge/axum-0.8-6E4AFF)](https://github.com/tokio-rs/axum)
[![AI-generated: primarily produced by an AI model](https://img.shields.io/static/v1?label=&message=AI-generated&color=red)](https://nasa-ammos.github.io/slim/?search=Badges)

> [!CAUTION]
> **Work in progress.** Repsetarr is unfinished and may change, break, or hand your
> *Arr apps a list you did not mean to build. Use it at your own risk, and look at what
> a list actually contains before you let Radarr start grabbing it.

> [!WARNING]
> **This project was vibe-coded.** That's OK, if it's used wisely, properly tested by a human
> and nothing critical depends on it. This tool is for \*Arr enthusiasts on a private network,
> not a product with an SLA. There is no login, no accounts, and no hardening for the public
> internet. Run it on a LAN, VPN, or reverse proxy you already trust - the same way you run
> Radarr and Sonarr.

Set algebra for movie and show lists.

Repsetarr reads lists from elsewhere - MDBList, a Radarr or Sonarr library, any JSON feed -
combines them with union, difference, intersection and symmetric difference, and serves the
result back in the exact shapes Radarr, Sonarr and Kometa know how to consume. It owns no
library, writes to nothing, and holds no state you would miss: it is a pure function from
other people's lists to a new list.

```yaml
lists:
  wanted_movies:
    list_formula: "(trending | top250) - my_radarr - never"
```

That list is then an import list URL in Radarr, or a collection file URL in Kometa. There is
no UI - the API *is* the product.

## Quick start

```bash
docker compose build
docker compose up -d
```

On first boot a commented starter `config.yml` is written to `/config`. Edit it, and the
running container picks the change up on save - no restart. Then check what you built:

```bash
curl -s localhost:9797/api/lists | jq
```

Copy `.env.example` to `.env` first if you want to keep API keys out of the config file; the
compose file is written for Unraid-style `/mnt/cache/appdata/repsetarr` paths, so change the
volume if yours differ.

## The algebra

| Operator | Means | Example |
|---|---|---|
| `\|` or `+` | union - everything in either side | `trending \| top250` |
| `-` | difference - the left side, minus the right | `top250 - my_radarr` |
| `&` | intersection - only what is in both | `top250 & oscar_winners` |
| `^` | symmetric difference - in exactly one side | `list_a ^ list_b` |
| `( )` | brackets, nested as deep as you like | `(a \| b) - (c & d)` |

Precedence follows Python's set operators: `+` and `-` bind tightest, then `&`, then `^`,
then `|`. So `a | b - c` is `a | (b - c)`. Bracket anything non-obvious - nobody reading
your config a year from now will remember this table.

A list may reference another list by name, so expressions compose:

```yaml
lists:
  shortlist:
    list_formula: "trending | top250"
    limit: 100
  to_grab:
    list_formula: "shortlist - my_radarr"   # sees the first 100, after the limit
```

Names are bare words. Because `-` is an operator, **a name containing a dash must be
quoted**: `"top-250" - my_radarr`. Underscores need no quoting and are easier to live with.

### What counts as the same item

Two items from different sources are the same item when they **share any external id**
(TMDb, IMDb, TVDb or Trakt), compared separately for movies and shows. Matching is
transitive: if one source knows `tmdb:550` is also `tt0137523`, then a TMDb-only entry and an
IMDb-only entry from two other sources collapse into one.

This matters because sources disagree about which ids they carry. MDBList gives you TMDb and
IMDb, Sonarr gives you TVDb and IMDb, a homemade feed might give you IMDb alone. Matching on
one provider would quietly leave duplicates in a union and leave items behind in a
difference. Nothing is ever matched by title or year.

Identity is resolved across **every** configured source at once, not per list. A source that
bridges two id namespaces therefore merges entries in a list that does not reference it - which
is what you want, since whether two ids name the same film is a fact about the world rather
than about the list you happen to be looking at. It only ever merges genuine duplicates, so a
count can fall when you add a source, never rise.

Items that arrive with no ids at all are dropped - there is nothing to match them on.

## Configuration

Everything lives in one file, `/config/config.yml` (`REPSETARR_CONFIG` to move it). Any
value may contain `${VAR}` or `${VAR:-fallback}`, so keys stay in the environment. Comments
are never interpolated.

### Sources

| Type | What it reads | Key options |
|---|---|---|
| `mdblist` | An MDBList list | `list: user/name`, or `list_id:`, or a pasted `url:`; `apikey:`, `media_type:`, `limit:`, `ttl:` |
| `radarr` | Every movie in a Radarr instance | `url:`, `api_key:`, `monitored_only:` |
| `sonarr` | Every series in a Sonarr instance | `url:`, `api_key:`, `monitored_only:` |
| `json` | Any JSON endpoint | `url:`, `headers:`, `path:` (dotted path to the array), `fields:` (key names per id), `media_type:` |
| `static` | Ids written in the config | `items: ["tmdb:550", "imdb:tt0133093"]`, `media_type:` |

```yaml
providers:
  mdblist:
    apikey: ${MDBLIST_API_KEY}

sources:
  trending:
    type: mdblist
    list: someuser/trending-movies
    ttl: 12h
  my_radarr:
    type: radarr
    url: http://radarr:7878
    api_key: ${RADARR_API_KEY}
  stevenlu:
    type: json
    url: https://popular-movies-data.stevenlu.com/movies.json
    media_type: movie
    fields:
      imdb: imdb_id
  never:
    type: static
    media_type: movie
    items: ["tmdb:550"]
```

`json` field names default to the spellings these feeds usually use (`tmdbId`, `tmdb_id`,
`tmdb`, `imdbId`, `imdb_id`, …), so `fields:` is only needed for a feed that invents its own.

### Lists

| Option | Default | What it does |
|---|---|---|
| `list_formula` | *(required)* | The set algebra over sources and lists |
| `filter` | - | Boolean algebra over names from `filters:` (see below) |
| `media_type` | `any` | Keep only `movie` or only `show` |
| `min_year` / `max_year` | - | Filter on release year |
| `released_after` / `released_before` | - | Filter on release date (`YYYY-MM-DD`) |
| `sort` | `none` | `none`, `rank`, `title`, `year`, `released`, `random` |
| `order` | `asc` | `asc` or `desc` |
| `limit` | - | Keep the first N after sorting |
| `kometa.collection` | *(list name)* | Collection name in the generated YAML |
| `kometa.sync_mode` | `sync` | Passed through to Kometa |
| `kometa.collection_order` | `custom` | Passed through to Kometa |
| `kometa.extra` | - | Any extra keys copied into the collection block |

Filters, sort and limit run **after** the algebra, and a list referenced from another
expression contributes its post-processed contents. With `sort: none` the order is the one
the algebra produced: a union keeps the left side's order and appends what is new on the
right, so a source's own ranking survives.

### Filters

The algebra decides *which lists* an item comes from. Filters decide *what the item is*.
They are two separate languages on purpose: a set operand is a finite collection, a filter is
a predicate with no extent of its own, and writing `russian` where a source name belongs
would read like a set that has to be fetched from somewhere.

Define named blocks under `filters:`, then apply them per list with `filter:`:

```yaml
filters:
  russian:
    country: ru, su          # comma-separated values are alternatives
  kids_safe:
    content_rating: G, PG    # every line in a block must hold
    runtime.lte: 100
  animation:
    genre: animation

lists:
  ru_kids:
    list_formula: "curated_3_5 & russian_content"   # set algebra
    filter: "russian and (kids_safe or animation)"  # boolean algebra
```

Note that `filter:` holds an *expression*, not a single filter name: `and`, `or`, `not` and
brackets, with `not` binding tightest and `and` before `or`. `&&`, `||` and `!` work too; a
bare `&` or `|` is rejected with a note that it belongs in `list_formula:`.

**Attributes and their modifiers.** Naming follows Kometa/TMDb, so `content_rating` and
`original_language` are spelled the way you have seen them elsewhere.

| Attribute | Modifiers |
|---|---|
| `country`, `original_language`, `spoken_language`, `content_rating`, `status`, `type` | bare = any of, `.not` |
| `genre` | bare = any of, `.not`, `.all` |
| `runtime`, `year` | bare = equals, `.gt`, `.gte`, `.lt`, `.lte` |
| `release` | `.before`, `.after` (bare means `.after`) |
| `title` | bare = contains, `.not`, `.begins`, `.ends`, `.regex` |

`language`, `genres`, `certification`, `released` and `media_type` are accepted as aliases.
An unknown attribute or modifier is a config error, reported with the alternatives.

**When the attribute is not known.** A line whose attribute the item does not carry answers
`unknown:`, which defaults to `exclude` - the same way `min_year` already drops items that
have no year. Set `unknown: include` per block to keep them instead. Note the consequence:
`country.not: us` fails for an item of unknown country, because neither "it is US" nor "it is
not US" can be shown; but `not russian` in a list's `filter:` *admits* that item, because the
inner block was false. Two-valued logic, no surprises hiding in a third state.

### Where attributes come from

MDBList list items already carry `country`, `language`, `spoken_language`, `runtime`,
`status` and (on request) `genres` in the payload Repsetarr fetches anyway, so filtering an
MDBList-backed list costs **nothing extra**. Items merged from several sources share what
any one of them knew: a title that is both in an MDBList list and in your Radarr library
gets its country from the former.

Everything else - Radarr, Sonarr, static blocks, JSON feeds without the fields - is filled in
by a background enricher, batched 200 ids to a request against MDBList's media-info endpoint
and remembered in `<cache.dir>/metadata.json`.

Three things keep that cheap:

- only attributes some `filter:` actually reads are fetched, so a config with no `filters:`
  makes no requests at all;
- only sources feeding a filtered list are considered;
- ids that came back empty are remembered as such, so a title the provider has never heard of
  is not asked about again tomorrow.

A cold 5,000-title Radarr library costs about 25 requests, once; steady state is zero. The
default TTL is 30 days because a film's country of origin does not change.

```yaml
metadata:
  enabled: true
  ttl: 30d          # positive answers
  miss_ttl: 7d      # "never heard of it" answers
  budget: 2000      # most requests one enrichment pass may make
  reserve: 1000     # stop when the daily allowance drops below this
```

It needs `providers.mdblist.apikey`. Nothing is ever fetched while serving a request: a list
whose metadata is not warm yet is answered from what is known, and the response says so with
`X-Repsetarr-Unenriched`.

Two things worth knowing about the data itself. MDBList reports **one** country per title, so
a co-production shows whichever it picked; and Soviet-era films are filed under `su`, not
`ru` - hence `country: ru, su`. Filtering on `original_language` is often steadier.

### Server and cache

```yaml
server:
  host: 0.0.0.0
  port: 9797
cache:
  dir: /config/cache
  default_ttl: 6h        # for sources that set no ttl of their own
  check_interval: 1m     # how often to look for sources that aged out
  persist: true
```

### Environment

| Variable | Default | Purpose |
|---|---|---|
| `REPSETARR_CONFIG` | `/config/config.yml` | Config file location |
| `LOG_LEVEL` | `info` | `error` \| `warn` \| `info` \| `debug` \| `trace` |
| `RUST_LOG` | - | Full tracing filter; overrides `LOG_LEVEL` |
| `PUID` / `PGID` | `99` / `100` | User the process runs as (Unraid `nobody:users`) |
| `UMASK` | `002` | File creation mask |
| `TZ` | `Etc/UTC` | Container timezone |
| `HEALTH_URL` | `http://127.0.0.1:9797/api/health` | Used by the container healthcheck; change it if you change the port |

## API

| Endpoint | Serves |
|---|---|
| `GET /api/health` | Version, uptime, and every source with its item count, age, staleness and last error |
| `GET /api/lists` | Every list with its expression, counts, sources and endpoint URLs |
| `GET /api/lists/{name}/radarr` | `[{"id": 550, …}]` - TMDb ids, movies only |
| `GET /api/lists/{name}/sonarr` | `[{"title": …, "tvdbId": …, "tmdbId": …, "imdbId": …}]` - shows only |
| `GET /api/lists/{name}/kometa.yml` | A Kometa collection file (`/kometa` works too) |
| `POST /api/reload` | Re-read the config file |

Responses carry `X-Repsetarr-Count`, `X-Repsetarr-Skipped` (items the consumer's format could
not represent), `X-Repsetarr-Stale` (sources serving data past its TTL) and
`X-Repsetarr-Unenriched` (candidates a filter had to judge without knowing every attribute it
reads - the answer is provisional until the enricher catches up).

An unknown list is a `404`. A list whose source has never been fetched successfully is a
`503` - the list is fine, the data behind it just is not here yet.

### Wiring it up

**Radarr** - Settings → Lists → **+** → **Custom Lists** (under Advanced), then

```
List URL: http://repsetarr:9797/api/lists/wanted_movies/radarr
```

Radarr's Custom Lists parser reads TMDb ids only, so movies with no TMDb id are left out;
the count is in `X-Repsetarr-Skipped` and in the log.

**Sonarr** - Settings → Import Lists → **+** → **Custom List**, then

```
URL: http://repsetarr:9797/api/lists/wanted_shows/sonarr
```

**Kometa** - in `config.yml`:

```yaml
libraries:
  Movies:
    collection_files:
      - url: http://repsetarr:9797/api/lists/wanted_movies/kometa.yml
```

Movies come out as `tmdb_movie`, shows as `tvdb_show`, and shows with no TVDb id as
`tmdb_show` in the same collection - Kometa unions the builders.

## Caching

Nothing is fetched while a request is being served. A background task refreshes each source
on its TTL and writes a snapshot to `/config/cache`, so a restart does not refetch anything
and a Radarr sync never waits on MDBList.

A failed refresh keeps the last good snapshot and marks the source stale rather than serving
an empty list - an empty list to Radarr with an import list set to remove-and-delete would be
an expensive way to learn that MDBList had a bad afternoon. The failure shows up in
`/api/health` and in `X-Repsetarr-Stale`. A snapshot whose source configuration has changed
is discarded rather than reused.

## Development

Rust 1.88 or newer (edition 2024; the code uses let-chains).

```bash
cargo test
cargo clippy --all-targets -- -D warnings
REPSETARR_CONFIG=./config.example.yml cargo run
```

| Path | What lives there |
|---|---|
| `src/expr/` | The expression language - lexer, precedence-climbing parser, evaluator |
| `src/identity.rs` | Union-find over external ids: what makes two items one item |
| `src/sources/` | One module per provider, behind a single fetch entry point |
| `src/cache.rs` | Snapshots, staleness, atomic writes to disk |
| `src/lists.rs` | Fetch → identify → evaluate → filter, sort, limit |
| `src/api.rs` | Routes and one renderer per consumer |
| `tests/` | End-to-end tests over the real router, with mocked upstreams |

The test suite asserts the Radarr and Sonarr payloads byte for byte against what those apps'
parsers actually read, so a well-meaning change to the JSON shape fails loudly.

## Contribution

Pull requests are not accepted at the moment. Suggestions, bug reports and issues are very
welcome.
