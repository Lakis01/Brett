# Preparation

Brett's preparation stage is built around a loaded PGN or SCID database. It
answers one practical question: **given the position on the board, what is this
opponent likely to play next?**

## Workflow

1. Open a database and switch to the **Players** tab.
2. Select a player. Brett shows their overall record, White/Black splits, peak
   and average Elo, year range, and the games in the current database.
3. Choose **Prepare as White** (you play White, the selected player is Black)
   or **Prepare as Black**.
4. Brett creates a scratch game with `[Event "Preparation"]` and opens the
   **Tree** tab. The normal database game is kept so that **Exit prep** can
   restore it.
5. Play your move. On the opponent's turn, the prediction panel lists candidate
   replies with probability, games seen, recent games, last-seen date, and
   trend. Click a row to play it, or play any other legal move on the board.
6. **Exit prep** returns to the game that was open before preparation started.

Preparation moves are ordinary game-tree edits. They stay in memory for the
session and are kept when switching between database games; they are not
written back to the source database.

## Player identity

Player names in PGN and SCID databases are inconsistently formatted. Brett
normalises a name by splitting on non-alphanumeric characters, lowercasing the
words, sorting them, and removing duplicates. Punctuation and word order
therefore do not matter:

```text
"Fischer, Robert James" == "Robert James Fischer" == "Robert  James Fischer"
```

An empty name, or a lone `?`, identifies no player. The normalized key is used
for player records, game lists, and the per-player book. The displayed name is
the first spelling encountered in the database.

## Player book

The **Players** tab itself is built from headers only, so it is available
immediately after a database is opened. A player book is richer and is built on
demand in a background thread; the UI shows progress while it works.

For each game in which the selected player appears:

- the mainline is followed (variations are analysis, not tournament history);
- the book records the position before each move made by the selected player;
- the move, game result from that player's perspective, date, and recency
  weight are stored;
- the walk stops at the first repeated position, null move, illegal move, or
  malformed game.

Only games in the loaded database are used. A book is not yet merged across
several databases.

## Prediction model

The model is deliberately small and inspectable. It is a time-decayed
Dirichlet estimate with two components.

### Candidate moves

At a position, candidates are the union of:

- moves played in the whole database at this position, and
- moves this player played at this position.

For each database move, Brett subtracts the player's own games before using the
remaining database frequency as a prior. This prevents the opponent's games
from being counted twice.

### Recency weights

A game receives the weight

```text
w = 0.5 ^ (age_days / half_life_days)
```

where `age_days` is measured from today. Games without a usable date receive a
fixed small weight. The default half-life is **547.5 days (18 months)**.

The same game also belongs to the recent regime when its age is at most
**730 days (24 months)**. Recent weights are summed separately.

### Prior and mixture

For each candidate move:

```text
alpha = prior_games * database_share + smoothing
P_decayed = (alpha + player_weight) / (sum(alpha) + sum(player_weight))
P_recent  = (alpha + player_recent_weight) / (sum(alpha) + sum(player_recent_weight))
P         = (1 - mixture) * P_decayed + mixture * P_recent
mixture   = recent_weight / (recent_weight + recent_mix_games)
```

Defaults:

| Parameter | Value | Meaning |
| --- | ---: | --- |
| `half_life_days` | 547.5 | 18-month exponential half-life |
| `recent_days` | 730 | 24-month recent window |
| `unknown_date_weight` | 0.20 | weight of an undated game |
| `prior_games` | 3.0 | strength of the database prior |
| `smoothing` | 0.05 | additive smoothing per candidate |
| `recent_mix_games` | 2.0 | recent pseudo-games needed for half mixture |

If the player has no observed moves at the position, the model falls back to
the database prior and marks the row as **database prior only**. If neither the
player nor the database has a move, the panel says so.

The **trend** column compares the move's share inside the recent window with its
share outside that window, in percentage points. It is only shown when there is
enough recent and historical data to make the comparison meaningful. Trend is
descriptive; it is not fed back into the probability.

### What the number means

The probability is a calibrated estimate from observed choices, not an engine
evaluation and not a guarantee. Small samples are dominated by the database
prior; a player with a short or undated history will produce wide uncertainty
even though the UI displays a single percentage. The prior uses only games in
the loaded database.

## Online imports

**File → Import from Lichess…** and **File → Import from Chess.com…** fetch
public games for a username through unauthenticated public APIs.

- The import is capped at **5,000 games**.
- Lichess returns the requested export in one request.
- Chess.com has no bulk PGN endpoint. Brett combines up to **36 recent monthly
  archives**.
- The result is cached under `$XDG_CACHE_HOME/brett/online/` (or
  `~/.cache/brett/online/`) and opened as a temporary database.
- No password, OAuth token, or cookie is read or sent. The username is sent to
  the selected site, so use a public account. Network access is required.
- Imported games are not merged into the current database and nothing is
  written next to a source database.

## Threading and limits

- Header indexing for the player directory runs on the UI thread when a
  database is opened. It is cheap because it does not build move trees.
- Position indexing and player-book construction run in background threads and
  report progress through channels.
- The prediction panel is recomputed from the in-memory book and position
  index; it does not call a chess engine.
- Player books are currently per database. Merging several reference files and
  adding engine/cloud evaluation are future milestones.
