//! bash support for dynamic completion: `clap_complete`'s own bash adapter, except that
//! the candidates it prints are made safe to insert into a command line.
//!
//! bash inserts a candidate exactly as printed (the registration script does not use
//! `-o filenames`), so a field name such as `` `my-field` `` (a command substitution!), or
//! one with spaces, `$` or `;`, would change the meaning of the line, and a name coming
//! from a document is attacker-controlled data. Escaping needs to know whether the cursor
//! is inside an open quote, which bash does not tell a completion function; the
//! registration script is therefore patched to pass the line up to the cursor in
//! [`LINE_VAR`].

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;

use clap_complete::engine::complete;
use clap_complete::env::{Bash, EnvCompleter};
use itertools::Itertools;

/// The environment variable the patched registration script sets to the part of the
/// command line before the cursor.
const LINE_VAR: &str = "_FSQ_COMP_LINE";

/// The line of `clap_complete`'s bash script after which [`LINE_VAR`] is passed. A unit
/// test fails if a `clap_complete` upgrade changes it.
const ANCHOR: &str = "_CLAP_COMPLETE_INDEX=\"$_CLAP_COMPLETE_INDEX\" \\\n";

/// Characters that never need quoting in a bash word.
fn is_plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_./:,=-".contains(c)
}

/// The quote the cursor is inside of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    Single,
    Double,
}

/// The quote still open at the end of `line` (the command line up to the cursor), as bash
/// would read it.
fn open_quote(line: &str) -> Option<Quote> {
    line.chars()
        .fold((None, false), |(open, escaped), c| {
            match (open, escaped, c) {
                (None | Some(Quote::Double), false, '\\') => (open, true),
                (None, false, '\'') => (Some(Quote::Single), false),
                (None, false, '"') => (Some(Quote::Double), false),
                (Some(Quote::Single), false, '\'') | (Some(Quote::Double), false, '"') => {
                    (None, false)
                }
                // Anything else, including the character after a backslash, changes nothing.
                _ => (open, false),
            }
        })
        .0
}

/// `candidate` as one bash word inserted at the cursor, or `None` if it cannot be inserted
/// safely (it has a control character). Outside quotes it is single-quoted unless it only
/// has plain characters; inside an open quote only that quote's metacharacters are escaped.
fn bash_word(candidate: &str, open: Option<Quote>) -> Option<String> {
    if candidate.chars().any(char::is_control) {
        return None;
    }
    let single = |s: &str| s.replace('\'', r"'\''");
    Some(match open {
        Some(Quote::Single) => single(candidate),
        Some(Quote::Double) => candidate
            .chars()
            .flat_map(|c| {
                let escape = matches!(c, '\\' | '"' | '$' | '`').then_some('\\');
                escape.into_iter().chain([c])
            })
            .collect(),
        None if candidate.chars().all(is_plain) => candidate.to_owned(),
        None => format!("'{}'", single(candidate)),
    })
}

/// `clap_complete`'s bash adapter with escaped candidates.
#[derive(Debug, Clone, Copy)]
pub(super) struct FsqBash;

impl EnvCompleter for FsqBash {
    fn name(&self) -> &'static str {
        Bash.name()
    }

    fn is(&self, name: &str) -> bool {
        Bash.is(name)
    }

    fn write_registration(
        &self,
        var: &str,
        name: &str,
        bin: &str,
        completer: &str,
        buf: &mut dyn Write,
    ) -> io::Result<()> {
        let mut script = Vec::new();
        Bash.write_registration(var, name, bin, completer, &mut script)?;
        let script = String::from_utf8_lossy(&script).replace(
            ANCHOR,
            &format!("{ANCHOR}        {LINE_VAR}=\"${{COMP_LINE:0:COMP_POINT}}\" \\\n"),
        );
        buf.write_all(script.as_bytes())
    }

    fn write_complete(
        &self,
        cmd: &mut clap::Command,
        args: Vec<OsString>,
        current_dir: Option<&Path>,
        buf: &mut dyn Write,
    ) -> io::Result<()> {
        let index = std::env::var("_CLAP_COMPLETE_INDEX")
            .ok()
            .and_then(|i| i.parse().ok())
            .unwrap_or_default();
        let ifs = std::env::var("_CLAP_IFS").unwrap_or_else(|_| "\n".to_owned());
        let open = std::env::var(LINE_VAR)
            .ok()
            .and_then(|line| open_quote(&line));
        let words = complete(cmd, args, index, current_dir)?
            .iter()
            .filter_map(|c| bash_word(&c.get_value().to_string_lossy(), open))
            .join(&ifs);
        buf.write_all(words.as_bytes())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn open_quote_follows_bash_quoting() {
        assert_eq!(open_quote("fsq query users"), None);
        assert_eq!(open_quote("fsq --where 'age"), Some(Quote::Single));
        assert_eq!(open_quote("fsq --where 'age == 1' --sel"), None);
        assert_eq!(open_quote(r#"fsq --where "a"#), Some(Quote::Double));
        assert_eq!(open_quote(r#"fsq --where "a \" b"#), Some(Quote::Double));
        assert_eq!(open_quote(r"fsq a\'b"), None);
        assert_eq!(open_quote(r#"fsq 'it"s"#), Some(Quote::Single));
        assert_eq!(open_quote(r#"fsq "it's""#), None);
        assert_eq!(open_quote(r"fsq 'a\"), Some(Quote::Single));
    }

    #[test]
    fn plain_words_are_left_alone() {
        for plain in [
            "users",
            "--where",
            "a.b_c",
            "a,b",
            "--project=p",
            "x-y",
            "a:b",
        ] {
            assert_eq!(bash_word(plain, None).as_deref(), Some(plain));
        }
    }

    #[test]
    fn unquoted_candidates_are_single_quoted() {
        let word = |s| bash_word(s, None).unwrap();
        assert_eq!(word("`my-field`"), "'`my-field`'");
        assert_eq!(word("a b"), "'a b'");
        assert_eq!(word("$(rm -rf ~)"), "'$(rm -rf ~)'");
        assert_eq!(word("a;b&c|d"), "'a;b&c|d'");
        assert_eq!(word("it's"), r"'it'\''s'");
        assert_eq!(word("`a'b`"), r"'`a'\''b`'");
        assert_eq!(word("(default)"), "'(default)'");
    }

    #[test]
    fn candidates_inside_an_open_quote_escape_only_that_quote() {
        let single = |s| bash_word(s, Some(Quote::Single)).unwrap();
        assert_eq!(single("`my-field` == x"), "`my-field` == x");
        assert_eq!(single("it's"), r"it'\''s");
        let double = |s| bash_word(s, Some(Quote::Double)).unwrap();
        assert_eq!(double("`my-field` $x"), r"\`my-field\` \$x");
        assert_eq!(double(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(double("plain"), "plain");
    }

    #[test]
    fn control_characters_drop_the_candidate() {
        for open in [None, Some(Quote::Single), Some(Quote::Double)] {
            assert_eq!(bash_word("a\nb", open), None);
            assert_eq!(bash_word("a\u{b}b", open), None);
            assert_eq!(bash_word("a\u{1b}[31m", open), None);
        }
    }

    /// The escaped word, evaluated by a real bash, is the candidate again.
    #[test]
    fn escaped_words_round_trip_through_bash() {
        let Ok(bash) = std::process::Command::new("bash").arg("--version").output() else {
            return;
        };
        assert!(bash.status.success());
        for candidate in [
            "`my-field`",
            "a b",
            "$(echo pwned)",
            "it's",
            "a;b",
            "x\\y",
            "(default)",
        ] {
            let word = bash_word(candidate, None).unwrap();
            let out = std::process::Command::new("bash")
                .args(["-c", &format!("printf %s {word}")])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), candidate);
        }
    }

    #[test]
    fn registration_passes_the_line_up_to_the_cursor() {
        let mut script = Vec::new();
        FsqBash
            .write_registration("FSQ_COMPLETE", "fsq", "fsq", "/bin/fsq", &mut script)
            .unwrap();
        let script = String::from_utf8(script).unwrap();
        assert!(script.contains("_FSQ_COMP_LINE=\"${COMP_LINE:0:COMP_POINT}\""));
        assert!(script.contains("FSQ_COMPLETE=\"bash\""));
        assert!(script.contains("_CLAP_COMPLETE_INDEX"));
    }
}
