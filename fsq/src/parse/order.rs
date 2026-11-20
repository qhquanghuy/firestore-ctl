//! `--order-by` terms: `field` or `field:asc|desc`.

use std::str::FromStr;

use super::{ParseError, unquoted};
use crate::domain::{Direction, FieldPath, OrderBy};

impl FromStr for OrderBy {
    type Err = ParseError;

    /// `field` sorts ascending; `field:asc` / `field:desc` is explicit. The direction
    /// follows the last `:` outside backticks, so quote fields that contain one.
    fn from_str(s: &str) -> Result<Self, ParseError> {
        let s = s.trim();
        let (field, direction) = match unquoted(s, '`').filter(|&(_, c)| c == ':').last() {
            None => (s, Direction::Asc),
            Some((colon, _)) => (&s[..colon], direction(s, s[colon + 1..].trim())?),
        };
        let field: FieldPath = field
            .trim()
            .parse()
            .map_err(|e| ParseError::new(s, format!("{e}")))?;
        Ok(Self { field, direction })
    }
}

fn direction(input: &str, text: &str) -> Result<Direction, ParseError> {
    match text {
        t if t.eq_ignore_ascii_case("asc") => Ok(Direction::Asc),
        t if t.eq_ignore_ascii_case("desc") => Ok(Direction::Desc),
        t => Err(ParseError::new(
            input,
            format!(
                "unknown direction `{t}`; use asc or desc (backtick-quote fields containing `:`)"
            ),
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::Direction;

    #[test]
    fn order_by() {
        assert_eq!(
            "createdAt:desc".parse::<OrderBy>().map(|o| o.direction),
            Ok(Direction::Desc)
        );
        assert_eq!(
            "createdAt".parse::<OrderBy>().map(|o| o.direction),
            Ok(Direction::Asc)
        );
    }

    #[test]
    fn explicit_asc_and_nested_and_quoted_fields() {
        assert_eq!(
            "a.b:asc"
                .parse::<OrderBy>()
                .map(|o| (o.field.to_string(), o.direction)),
            Ok(("a.b".into(), Direction::Asc))
        );
        let o: OrderBy = "`a:b`:desc".parse().unwrap();
        assert_eq!(
            (o.field.segments().to_vec(), o.direction),
            (vec!["a:b".to_owned()], Direction::Desc)
        );
        let o: OrderBy = "`a:b`".parse().unwrap();
        assert_eq!(o.field.segments(), ["a:b"]);
    }

    #[test]
    fn surrounding_spaces_are_ignored() {
        let o: OrderBy = " name : desc ".parse().unwrap();
        assert_eq!(
            (o.field.segments().to_vec(), o.direction),
            (vec!["name".to_owned()], Direction::Desc)
        );
    }

    #[test]
    fn rejects_bad_directions_and_fields() {
        assert!("a:up".parse::<OrderBy>().is_err());
        assert!("a:".parse::<OrderBy>().is_err());
        assert!(":desc".parse::<OrderBy>().is_err());
        assert!("".parse::<OrderBy>().is_err());
    }
}
