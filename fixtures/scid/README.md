# SCID fixtures

`opera.si4`, `opera.sn4`, and `opera.sg4` are a SCID4 database generated from
the public-domain Opera Game fixture in [`../opera.pgn`](../opera.pgn).

They are committed so the native SCID reader is tested without depending on a
local database. The binary files are generated data, not copied SCID source
code. To regenerate them:

```sh
mkdir -p /tmp/opencode/scid-opera
cp fixtures/opera.pgn /tmp/opencode/scid-opera/opera.pgn
cd /tmp/opencode/scid-opera
pgnscid -f opera.pgn
cp opera.si4 opera.sn4 opera.sg4 /path/to/Brett/fixtures/scid/
```

The PGN includes one comment at the start, one after `2...d6`, one inside the
`3...exd4` variation, and a `!` NAG on `10.Nxb5`, so the fixture exercises
comments, variations, and annotation decoding.
