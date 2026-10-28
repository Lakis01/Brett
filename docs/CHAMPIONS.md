# World champions and the crown badge

Brett marks recognized world champions with a gold crown (👑) in the board
strips, the Players list, and the selected-player header. The crown is a
display aid, not a claim about a player's current title or rating.

Two lineages are available under **Settings → Champion lineage**:

| Setting | Scope |
| --- | --- |
| **FIDE/inclusive** (default) | The classical/PCA line plus the four FIDE knockout champions of 1999–2006. |
| **Classical/PCA** | The uninterrupted line from Steinitz to the present, excluding the knockout champions. |

Both lineages cover the full history back to Wilhelm Steinitz. The four
FIDE-only names are Alexander Khalifman, Ruslan Ponomariov, Rustam
Kasimdzhanov, and Veselin Topalov. Anatoly Karpov and Viswanathan Anand are
already part of the classical line, so they are not duplicated.

## Classical/PCA line

| Champion | Reign |
| --- | --- |
| Wilhelm Steinitz | 1886–1894 |
| Emanuel Lasker | 1894–1921 |
| José Raúl Capablanca | 1921–1927 |
| Alexander Alekhine | 1927–1935, 1937–1946 |
| Max Euwe | 1935–1937 |
| Mikhail Botvinnik | 1948–1957, 1958–1960, 1961–1963 |
| Vasily Smyslov | 1957–1958 |
| Mikhail Tal | 1960–1961 |
| Tigran Petrosian | 1963–1969 |
| Boris Spassky | 1969–1972 |
| Bobby Fischer | 1972–1975 |
| Anatoly Karpov | 1975–1985 |
| Garry Kasparov | 1985–2000 |
| Vladimir Kramnik | 2000–2007 |
| Viswanathan Anand | 2007–2013 |
| Magnus Carlsen | 2013–2023 |
| Ding Liren | 2023–2024 |
| Gukesh Dommaraju | 2024–present |

## FIDE-only additions

| Champion | Reign |
| --- | --- |
| Alexander Khalifman | 1999–2000 |
| Ruslan Ponomariov | 2002–2004 |
| Rustam Kasimdzhanov | 2004–2005 |
| Veselin Topalov | 2005–2006 |

## Matching rules

The table lives in [`src/champions.rs`](../src/champions.rs). Matching is
deliberately conservative:

- Accents are folded, and word order, punctuation, and capitalization are
  ignored. `Capablanca, José Raúl`, `Jose Raul Capablanca`, and
  `José Raúl Capablanca` all resolve to the same champion.
- Common spellings and aliases are listed explicitly, including `Bobby
  Fischer` / `Robert J. Fischer`, `Garry` / `Gary Kasparov`, `Vishy Anand`,
  and `Gukesh D` / `D Gukesh` / `Gukesh Dommaraju`.
- A name that is not in the table is never crowned. In particular, a similar
  name such as `Magnus Carlsen Jr` does not inherit the crown.

The crown is independent of the optional photo pack. It also works when no
photos are installed, and the lineage setting persists in `display.json`.

## Crown tooltip

Hovering the crown shows the champion's title line, reign, and one factual
note. The text is generated from the same table as the crown, so a corrected
reign updates every surface at once.

Examples:

- **Magnus Carlsen** — World Champion 2013–2023. He beat Viswanathan Anand in
  2013 and won all five of his World Championship matches before choosing not
  to defend in 2023.
- **Garry Kasparov** — Classical World Champion 1985–2000. He formed the PCA
  in 1993.
- **Alexander Khalifman** — FIDE World Champion 1999–2000. He won the 1999
  FIDE knockout championship in Las Vegas.

The title line is deliberately precise: the four FIDE-only champions are
labelled **FIDE World Champion**, Kasparov and Kramnik are labelled **Classical
World Champion** for the split era, and the other champions are labelled
**World Champion**. "FIDE World Champion" is not used for the pre-1948 title or
for the classical line during the 1993–2006 split.
