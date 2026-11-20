use std::str::FromStr;

use super::{ParseError, parse_field, split_around, unquoted};
use crate::domain::{Direction, OrderBy};

impl FromStr for OrderBy {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let trimmed = s.trim();
        let colon = unquoted(trimmed, '`')
            .filter(|&(_, c)| c == ':')
            .last()
            .and_then(|(at, _)| split_around(trimmed, at));
        let (field, direction) = match colon {
            None => (trimmed, Direction::Asc),
            Some((field, _, text)) => (field, direction(s, text.trim())?),
        };
        let field = parse_field(
            field,
            "use `field:desc` for direction; backtick-quote field names with spaces or punctuation",
        )
        .map_err(|reason| ParseError::new(s, reason))?;
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
    fn space_separated_direction_is_rejected_with_a_hint() {
        let e = "createdAt desc".parse::<OrderBy>().unwrap_err();
        assert!(e.reason.contains("field:desc"), "{e}");
        assert!("my-field".parse::<OrderBy>().is_err());
        assert!("`my-field`:desc".parse::<OrderBy>().is_ok());
    }

    #[test]
    fn rejects_bad_directions_and_fields() {
        assert!("a:up".parse::<OrderBy>().is_err());
        assert!("a:".parse::<OrderBy>().is_err());
        assert!(":desc".parse::<OrderBy>().is_err());
        assert!("".parse::<OrderBy>().is_err());
    }
}
