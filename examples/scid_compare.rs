use std::collections::BTreeMap;

use brett::scid::ScidLibrary;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(left), Some(right)) = (args.first(), args.get(1)) else {
        anyhow::bail!("usage: scid_compare left.si4 right.si4");
    };
    let left = ScidLibrary::open(left)?;
    let right = ScidLibrary::open(right)?;
    assert_eq!(left.len(), right.len(), "game count changed");
    for index in 0..left.len() {
        let mut a: BTreeMap<String, String> = left
            .load(index)?
            .headers
            .into_iter()
            .filter(|(key, _)| !key.eq_ignore_ascii_case("PlyCount"))
            .collect();
        let mut b: BTreeMap<String, String> = right
            .load(index)?
            .headers
            .into_iter()
            .filter(|(key, _)| !key.eq_ignore_ascii_case("PlyCount"))
            .collect();
        // SCID stores the board as a suffix of the round name. Older records
        // can carry an explicit Board tag instead; normalize both shapes.
        if let Some(board) = a.remove("Board").or_else(|| b.remove("Board")) {
            let round = a
                .entry("Round".to_string())
                .or_insert_with(|| "?".to_string());
            if !round.ends_with(&format!(".{board}")) {
                if round.is_empty() || round == "?" {
                    *round = format!("?.{board}");
                } else {
                    round.push('.');
                    round.push_str(&board);
                }
            }
        }
        if a != b {
            println!("game {} differs", index + 1);
            for key in a
                .keys()
                .chain(b.keys())
                .collect::<std::collections::BTreeSet<_>>()
            {
                if a.get(key) != b.get(key) {
                    println!("  {key}: {:?} -> {:?}", a.get(key), b.get(key));
                }
            }
            anyhow::bail!("headers changed");
        }
    }
    println!("{} games: headers match", left.len());
    Ok(())
}
