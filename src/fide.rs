//! FIDE player-ID presentation helpers.
//!
//! PGN `WhiteFideId`/`BlackFideId` tags and imported metadata are free-form
//! strings. Only a trimmed, purely numeric value is treated as a real FIDE ID:
//! the stored tag is never rewritten, and no ID is inferred from a name or a
//! rating.

/// Return the trimmed FIDE ID from a tag, or `None` when the value is absent,
/// empty, a placeholder, or not numeric.
pub fn from_tag(raw: Option<&str>) -> Option<&str> {
    let id = raw?.trim();
    (is_numeric(id) && id != "0").then_some(id)
}

/// True when every character is an ASCII digit and the value is not zero.
pub fn is_numeric(id: &str) -> bool {
    !id.is_empty() && id != "0" && id.bytes().all(|byte| byte.is_ascii_digit())
}

/// Official FIDE rating-profile URL, only for a real numeric ID.
pub fn profile_url(id: &str) -> Option<String> {
    let id = from_tag(Some(id))?;
    Some(format!("https://ratings.fide.com/profile/{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_trimmed_numeric_ids() {
        assert_eq!(from_tag(Some(" 1503014 ")), Some("1503014"));
        assert_eq!(from_tag(Some("1503014")), Some("1503014"));
    }

    #[test]
    fn rejects_missing_placeholder_and_non_numeric_values() {
        for raw in [
            None,
            Some(""),
            Some("   "),
            Some("?"),
            Some("0"),
            Some("N/A"),
            Some("15030a4"),
            Some("-1503014"),
        ] {
            assert_eq!(from_tag(raw), None, "raw FIDE id {raw:?}");
        }
    }

    #[test]
    fn builds_profile_urls_only_for_numeric_ids() {
        assert_eq!(
            profile_url("1503014").as_deref(),
            Some("https://ratings.fide.com/profile/1503014")
        );
        assert_eq!(
            profile_url(" 1503014 ").as_deref(),
            Some("https://ratings.fide.com/profile/1503014")
        );
        assert_eq!(profile_url("Magnus"), None);
        assert_eq!(profile_url(""), None);
    }
}
