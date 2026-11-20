//! bash support for dynamic completion: `clap_complete`'s own bash adapter, except that
//! the candidates it prints are made safe to insert into a command line.
//!
//! bash inserts a candidate exactly as printed (the registration script does not use
//! `-o filenames`), so a field name such as `` `my-field` `` (a command substitution!), or
//! one with spaces, `$`, `;` or `!`, would change the meaning of the line, and a name
//! coming from a document is attacker-controlled data. Escaping needs to know whether the
//! cursor is inside an open quote, which bash does not tell a completion function; the
//! registration script is therefore patched to pass the line up to the cursor in
//! [`LINE_VAR`]. Without it (the patch did not apply, or something else registered the
//! completion) nothing but plain words is offered: the escaping fails closed.

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

/// Where the cursor is, as far as quoting goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cursor {
    /// Outside any quote.
    Bare,
    /// Inside `'...`.
    Single,
    /// Inside `"...`.
    Double,
    /// Not known: the command line was not passed.
    Unknown,
}

impl Cursor {
    /// The cursor at the end of `line` (the command line up to the cursor), as bash would
    /// read it; [`Cursor::Unknown`] without a line.
    fn after(line: Option<&str>) -> Self {
        line.map_or(Self::Unknown, |line| {
            line.chars()
                .fold((Self::Bare, false), |(open, escaped), c| {
                    match (open, escaped, c) {
                        (Self::Bare | Self::Double, false, '\\') => (open, true),
                        (Self::Bare, false, '\'') => (Self::Single, false),
                        (Self::Bare, false, '"') => (Self::Double, false),
                        (Self::Single, false, '\'') | (Self::Double, false, '"') => {
                            (Self::Bare, false)
                        }
                        // Anything else, including the character after a backslash, changes
                        // nothing.
                        _ => (open, false),
                    }
                })
                .0
        })
    }
}

/// `candidate` as the text to insert at the cursor, or `None` if it cannot be inserted
/// safely: it has a control character, or it is not a plain word and the cursor's quoting
/// is unknown. Outside quotes it is single-quoted unless it only has plain characters;
/// inside an open quote only that quote's metacharacters are escaped. `!` (history
/// expansion, which applies even inside single quotes) is always backslash-escaped from
/// outside quotes: the quote is closed around it and reopened.
fn bash_word(candidate: &str, cursor: Cursor) -> Option<String> {
    if candidate.chars().any(char::is_control) {
        return None;
    }
    let single = |s: &str| s.replace('\'', r"'\''").replace('!', r"'\!'");
    match cursor {
        _ if candidate.chars().all(is_plain) => Some(candidate.to_owned()),
        Cursor::Unknown => None,
        Cursor::Bare => Some(format!("'{}'", single(candidate))),
        Cursor::Single => Some(single(candidate)),
        Cursor::Double => Some(
            candidate
                .chars()
                .flat_map(|c| match c {
                    '\\' | '"' | '$' | '`' => vec!['\\', c],
                    '!' => vec!['"', '\\', '!', '"'],
                    _ => vec![c],
                })
                .collect(),
        ),
    }
}

/// The candidates that can be offered at the end of `line`, ready to insert.
fn bash_words<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
    line: Option<&str>,
) -> Vec<String> {
    let cursor = Cursor::after(line);
    candidates
        .into_iter()
        .filter_map(|candidate| bash_word(candidate, cursor))
        .collect()
}

/// `script`, `clap_complete`'s bash registration, passing the command line up to the
/// cursor to the binary. A script this does not recognise is returned as it is: it works,
/// and without the line only plain candidates are offered.
fn with_line_passed(script: &str) -> String {
    script.replace(
        ANCHOR,
        &format!("{ANCHOR}        {LINE_VAR}=\"${{COMP_LINE:0:COMP_POINT}}\" \\\n"),
    )
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
        buf.write_all(with_line_passed(&String::from_utf8_lossy(&script)).as_bytes())
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
        let line = std::env::var(LINE_VAR).ok();
        let candidates = complete(cmd, args, index, current_dir)?;
        let words = bash_words(
            candidates
                .iter()
                .map(|c| c.get_value().to_string_lossy().into_owned())
                .collect_vec()
                .iter()
                .map(String::as_str),
            line.as_deref(),
        );
        buf.write_all(words.join(&ifs).as_bytes())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn the_cursor_follows_bash_quoting() {
        let after = |line| Cursor::after(Some(line));
        assert_eq!(after("fsq query users"), Cursor::Bare);
        assert_eq!(after("fsq --where 'age"), Cursor::Single);
        assert_eq!(after("fsq --where 'age == 1' --sel"), Cursor::Bare);
        assert_eq!(after(r#"fsq --where "a"#), Cursor::Double);
        assert_eq!(after(r#"fsq --where "a \" b"#), Cursor::Double);
        assert_eq!(after(r"fsq a\'b"), Cursor::Bare);
        assert_eq!(after(r#"fsq 'it"s"#), Cursor::Single);
        assert_eq!(after(r#"fsq "it's""#), Cursor::Bare);
        assert_eq!(after(r"fsq 'a\"), Cursor::Single);
        assert_eq!(Cursor::after(None), Cursor::Unknown);
    }

    #[test]
    fn plain_words_are_left_alone_in_every_context() {
        for plain in [
            "users",
            "--where",
            "a.b_c",
            "a,b",
            "--project=p",
            "x-y",
            "a:b",
        ] {
            for cursor in [
                Cursor::Bare,
                Cursor::Single,
                Cursor::Double,
                Cursor::Unknown,
            ] {
                assert_eq!(bash_word(plain, cursor).as_deref(), Some(plain));
            }
        }
    }

    #[test]
    fn unquoted_candidates_are_single_quoted() {
        let word = |s| bash_word(s, Cursor::Bare).unwrap();
        assert_eq!(word("`my-field`"), "'`my-field`'");
        assert_eq!(word("a b"), "'a b'");
        assert_eq!(word("$(rm -rf ~)"), "'$(rm -rf ~)'");
        assert_eq!(word("a;b&c|d"), "'a;b&c|d'");
        assert_eq!(word("it's"), r"'it'\''s'");
        assert_eq!(word("`a'b`"), r"'`a'\''b`'");
        assert_eq!(word("(default)"), "'(default)'");
        assert_eq!(word("a!b"), r"'a'\!'b'");
    }

    #[test]
    fn candidates_inside_an_open_quote_escape_only_that_quote() {
        let single = |s| bash_word(s, Cursor::Single).unwrap();
        assert_eq!(single("`my-field` == x"), "`my-field` == x");
        assert_eq!(single("it's"), r"it'\''s");
        assert_eq!(single("a!b"), r"a'\!'b");
        let double = |s| bash_word(s, Cursor::Double).unwrap();
        assert_eq!(double("`my-field` $x"), r"\`my-field\` \$x");
        assert_eq!(double(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(double("a!b"), r#"a"\!"b"#);
        assert_eq!(double("plain"), "plain");
    }

    #[test]
    fn control_characters_drop_the_candidate() {
        for cursor in [
            Cursor::Bare,
            Cursor::Single,
            Cursor::Double,
            Cursor::Unknown,
        ] {
            assert_eq!(bash_word("a\nb", cursor), None);
            assert_eq!(bash_word("a\u{b}b", cursor), None);
            assert_eq!(bash_word("a\u{1b}[31m", cursor), None);
        }
    }

    #[test]
    fn an_unknown_cursor_offers_plain_words_only() {
        let candidates = [
            "users",
            "`my-field`",
            "a b",
            "name",
            "--where",
            "$(x)",
            "a!b",
        ];
        assert_eq!(bash_words(candidates, None), ["users", "name", "--where"]);
        // With the line, the same candidates are all offered, quoted for the context.
        assert_eq!(
            bash_words(candidates, Some("fsq query users --select ")),
            [
                "users",
                "'`my-field`'",
                "'a b'",
                "name",
                "--where",
                "'$(x)'",
                r"'a'\!'b'"
            ]
        );
    }

    /// What a real bash makes of `prefix`, the escaped candidate and `suffix`.
    fn evaluated(prefix: &str, word: &str, suffix: &str) -> String {
        let out = std::process::Command::new("bash")
            .args(["-c", &format!("printf %s {prefix}{word}{suffix}")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The escaped word, evaluated by a real bash in the context it was escaped for, is
    /// the candidate again (the surrounding quotes being what the user had typed).
    #[test]
    fn escaped_words_round_trip_through_bash() {
        let Ok(bash) = std::process::Command::new("bash").arg("--version").output() else {
            return;
        };
        assert!(bash.status.success());
        let candidates = [
            "`my-field`",
            "a b",
            "$(echo pwned)",
            "it's",
            "a;b",
            "x\\y",
            "(default)",
            "a!b",
            "say \"hi\" $HOME `id`",
        ];
        for candidate in candidates {
            let bare = bash_word(candidate, Cursor::Bare).unwrap();
            assert_eq!(evaluated("", &bare, ""), candidate, "bare {bare}");
            let single = bash_word(candidate, Cursor::Single).unwrap();
            assert_eq!(
                evaluated("'<", &single, ">'"),
                format!("<{candidate}>"),
                "single {single}"
            );
            let double = bash_word(candidate, Cursor::Double).unwrap();
            assert_eq!(
                evaluated("\"<", &double, ">\""),
                format!("<{candidate}>"),
                "double {double}"
            );
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

    #[test]
    fn an_unrecognised_script_is_left_as_it_is() {
        // The anchor is gone: nothing is patched, so no line will be passed, so only plain
        // words get offered (see `an_unknown_cursor_offers_plain_words_only`).
        let other = "_other() { :; }\ncomplete -F _other fsq\n";
        assert_eq!(with_line_passed(other), other);
        assert!(!with_line_passed(other).contains(LINE_VAR));
    }
}
