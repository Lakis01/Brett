# Preparation

Brett's preparation stage is built around a loaded PGN, SCID, or classic CBH
database. It answers one practical question: **given the position on the board,
what is this opponent likely to play next?**

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
   replies with posterior mean, approximate 90% credible interval, games seen,
   recent games, last-seen date, trend, and the matching database tree columns
   (games, W/D/L, score, Elo, years). A residual line reports legal moves
   absent from the data. Click a row to play it, or play any other legal move
   on the board.
6. On your turn, the panel shows the prepared player's moves from the current
   line so far: move, date, database score, and the likelihood assigned before
   the move was chosen. It is never empty.
7. The normal opening tree for the current position is always shown below the
   preparation section, in both players' turns.
8. **Recent games** lists the newest games in the player book that reached the
   current position. Set the limit (default **10**) in the section. Clicking a
   row opens a read-only game view; **Close preview** or Escape restores the
   same preparation position and tab.
9. **Exit prep** returns to the game that was open before preparation started,
   including the board side that was active before preparation.

An optional **Reference database prior** section on the selected-player page
configures a separate PGN, SCID, or classic CBH corpus for the background
distribution. The reference is indexed only for the prepared player's rating
window and cached away from the player's own position index. Without one, the
panel uses the current database prior and labels it as potentially skewed.

Preparation moves are ordinary game-tree edits. They stay in memory for the
session and are kept when switching between database games; they are not
written back to the source database.

## My player names and orientation

**Settings → My player names** stores the aliases you use, one per line,
highest priority first. Names are matched with the same normalisation as player
identity: punctuation and word order do not matter.

- The first alias is labelled **You** in the Players tab.
- In preparation, the first alias replaces the literal `You` in the PGN tag
  fields, so exported preparation games carry your real name. The board strip
  still labels your side **You** when no alias is configured; with an alias it
  shows the alias.
- When you open a game that contains one of the aliases, the board opens from
  that side. The first matching alias in list order wins. If no alias matches,
  the current orientation is kept.
- **Exit prep** and closing a recent-game preview restore the orientation that
  was active before the temporary view, not just the game and node.

## Preparation view

The right-hand panel has three parts in preparation mode:

- **Preparation section.** On the opponent's turn this is the likelihood list.
  Each row combines the player probability with the database tree statistics
  for the same move: games, score (from White's perspective), W/D/B, average
  Elo, and year range. On the user's turn it becomes the prepared player's move
  history in the current line. The likelihood for a past move is evaluated at
  the parent position, which is the same distribution that was shown before
  the move was chosen.
- **Opening tree.** The normal tree table for the current position is always
  present, even when the player book is empty. It can be collapsed but not
  hidden by the preparation state.
- **Recent games.** Newest-first games from the prepared player's book that
  reached the current position. The number is configurable from 1 to 50; the
  default is 10. Unknown dates sort last. Each row shows the game, date, the
  player's colour, result, and event.

Clicking a recent game loads that database game in a read-only preview. The
preparation game, its current node, and the right-hand tab are kept aside.
Board moves, tree clicks, tag editing, and annotation actions are disabled
while previewing. **Close preview** (or Escape) restores the preparation game
and exact cursor position. The recent-games preview does not mark the game
dirty and never writes to the source database.

## Player identity

Player names in PGN, SCID, and CBH databases are inconsistently formatted. Brett
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
- the book records every position the game reached, including the final
  position, so recent-game lookups work regardless of whose turn it is;
- the move, game result from that player's perspective, date, and recency
  weight are stored for every position where the selected player moved;
- each position also keeps a small reference to the source game, allowing the
  recent-games list to be built without rescanning the whole database;
- the walk stops at the first repeated position, null move, illegal move, or
  malformed game.

Only games in the loaded database are used. A book is not yet merged across
several databases.

## Prediction model

The model is deliberately small and inspectable. It combines a background
prior, the selected player's repertoire, recent form, and an explicit residual
for legal moves that were never observed. It is a hierarchical Dirichlet model,
not a single frequency count.

### Candidate moves

At a position, candidates are the union of:

- moves played in the background corpus at this position, and
- moves this player played at this position.

The background corpus is the configured **reference database** when it has
games at the position. Otherwise Brett falls back to the current database and
shows a warning that the prior may be skewed. For the current-database
fallback, the player's own games are subtracted from the background count so
the player is not used as their own evidence. A reference corpus is treated as
independent and is not adjusted.

Positions are keyed by a Zobrist hash, so transpositions share evidence. Color
is implicit: the side to move at a position fixes the color of the player who
moved from it, and the player book only records positions where the selected
player actually moved. Move popularity is the background count used both for
the candidate set and for the prior share.

### Recency weights

A game receives the weight

```text
w = 0.5 ^ (age_days / half_life_days)
```

where `age_days` is measured from today. Games without a usable date receive a
fixed small weight. The default half-life is **547.5 days (18 months)**.

The same game also belongs to the recent regime when its age is at most
**730 days (24 months)**. Recent weights are summed separately.

### Background strength and shrinkage

The background corpus is evidence in its own right. Its strength grows with
the number of background games, but is capped so that a very large reference
cannot erase the player's repertoire:

```text
B     = prior_games + min(sqrt(background_games), 12)
alpha = B * background_share + smoothing
```

A sparse background receives the `prior_games` floor. A large reference earns
up to about 15 pseudo-games of background strength. This is the explicit
sample-size shrinkage: the player's own games matter more when the background
is thin, and less when it is well supported.

### Unobserved legal moves

The candidate set contains only observed moves. Real players can choose other
legal moves, so the model reserves a residual:

```text
unseen_alpha = unseen_prior_games   if legal_moves > candidate_count
             = 0                    otherwise
```

This is why a move seen in **2/2** games is not displayed as 100%. A legal
position with 20 moves, one observed player move played twice, and no
background candidates still assigns roughly 13% to unobserved moves. When
every legal move has been observed, the residual is zero and the probabilities
sum to one.

### Mixture

For each candidate move:

```text
P_decayed = (alpha + player_weight) / (sum(alpha) + unseen_alpha + sum(player_weight))
P_recent  = (alpha + player_recent_weight) / (sum(alpha) + unseen_alpha + sum(player_recent_weight))
P         = (1 - mixture) * P_decayed + mixture * P_recent
mixture   = recent_weight / (recent_weight + recent_mix_games)
```

The unobserved residual is mixed the same way, so the displayed candidate
probabilities plus the **other legal moves** line sum to one.

Defaults:

| Parameter | Value | Meaning |
| --- | ---: | --- |
| `half_life_days` | 547.5 | 18-month exponential half-life |
| `recent_days` | 730 | 24-month recent window |
| `unknown_date_weight` | 0.20 | weight of an undated game |
| `prior_games` | 3.0 | floor of background-prior strength |
| `smoothing` | 0.05 | additive smoothing per candidate |
| `recent_mix_games` | 2.0 | recent pseudo-games needed for half mixture |
| `unseen_prior_games` | 0.75 | residual mass for unobserved legal moves |

### Intervals and confidence

Each candidate gets an approximate **90% credible interval**. The interval uses
the marginal Beta variance of the decayed and recent components; the mixture
variance is conservative because it ignores their positive covariance. The UI
shows the posterior mean as the filled part of the bar and the interval as a
gold segment across it.

The confidence label is deliberately sample-size based:

- **prior only** — no player games at this position;
- **low confidence** — 1–2 player games;
- **moderate confidence** — 3–8 player games;
- **good confidence** — 9–19 player games;
- **high confidence** — 20 or more player games.

A wide interval is a feature, not a bug. Small samples and unfamiliar
positions should look uncertain.

### Trend

The **trend** column compares the move's share inside the recent window with its
share outside that window, in percentage points. It is only shown when there is
enough recent and historical data to make the comparison meaningful. Trend is
descriptive; it is not fed back into the probability.

## Reference database

The selected-player page has a collapsible **Reference database prior**
section:

- **Path** — a PGN, SCID3/SCID4, or classic CBH database. It is opened
  independently of the player's database. If no path has been saved yet, Brett
  also honours the `BRETT_REFERENCE` environment variable.
- **Use a separate reference database** — enables or disables the reference.
- **Rating window ±** — default **200 Elo**.
- **Offset** — an explicit rating-scale mapping added to the player's average
  Elo before selecting the window. For example, `-75` targets a reference
  corpus whose ratings are about 75 points lower than the player's source;
  `+75` targets an overrated reference corpus.

The mapping is explicit:

```text
target = clamp(round(player_average_elo) + offset, 50, 4000)
window = [max(0, target - band), target + band]
```

A game enters the reference index when the mean of its available `WhiteElo`
and `BlackElo` tags is inside the window. Games without ratings are excluded
from a filtered reference build.

Reference indexing is optional and background-only:

1. Configure the path and click **Save & index for this player** (or start a
   preparation; Brett then prepares the window automatically).
2. The reference library is opened and its cache is checked. The cache key
   includes the reference source and the exact rating window.
3. A missing cache is built in a worker thread. The panel keeps using the
   current database fallback and says so while indexing.
4. Once ready, the panel uses the reference prior and shows the window, game
   count, and position count.

Reference indexes live under `$XDG_CACHE_HOME/brett/index/` with a tagged
filename. They can never be confused with the player's own index or another
player's rating window. Settings are stored in
`$XDG_CONFIG_HOME/brett/preparation.json`. Source databases are read-only; all
writes go to the cache directory.

If the reference is disabled, missing, fails to build, contains no rated games
in the window, or has no games at the current position, the panel falls back to
the current database prior and shows the warning. The fallback is never silent.

## Held-out evaluation

The example harness splits the selected player's games chronologically, builds
the player book and background statistics only from earlier games, and scores
the held-out moves. Games without usable dates remain in the book but cannot
take part in the chronological split; the report warns when the held-out set
has no usable dates:

```sh
cargo run --release --example predictor_eval -- database.pgn "Player Name" 20
```

The last argument is the number of held-out games (default: one fifth). The
report contains:

- **log loss** — lower is better;
- **Brier score** — multiclass squared error over the observed moves and the
  unobserved residual, with the residual spread uniformly over the remaining
  legal moves;
- **top-1** and **top-3** accuracy;
- **top-1 calibration curves** for the model and each available baseline
  (predicted probability versus observed hit rate).

It compares the full model with a frequency-only baseline and, when ratings
are available, an Elo-filtered frequency baseline built from the same training
games. These metrics measure the model on one corpus; they are not a promise
about a future game.

### What the number means

The probability is an estimate from observed choices, not an engine evaluation
and not a guarantee. Small samples are shrunk toward the background prior and
carry wide intervals. The residual line keeps the model from implying certainty
when the data simply contains no example of the alternatives.

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
