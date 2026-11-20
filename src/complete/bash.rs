//! `clap_complete`'s bash adapter, except that candidates are made safe to insert: bash inserts a
//! candidate exactly as printed, and a field name from a document is attacker-controlled (`` `x` `` is
//! a command substitution). Escaping needs to know whether the cursor is inside an open quote, which
//! bash does not tell a completion function, so the registration script is patched to pass the line
//! up to the cursor in [`LINE_VAR`]. Without it only plain words are offered: escaping fails closed.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;

use clap_complete::engine::complete;
use clap_complete::env::{Bash, EnvCompleter};
use itertools::Itertools;

const LINE_VAR: &str = "_FIRECTL_COMP_LINE";

/// Anchor in `clap_complete`'s bash script; a unit test fails if an upgrade changes it.
const ANCHOR: &str = "_CLAP_COMPLETE_INDEX=\"$_CLAP_COMPLETE_INDEX\" \\\n";

fn is_plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_./:,=-".contains(c)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cursor {
    Bare,
    Single,
    Double,
    Unknown,
}

impl Cursor {
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
                        _ => (open, false),
                    }
                })
                .0
        })
    }
}

/// `None` if the candidate has a control character, or is not a plain word and the cursor's quoting
/// is unknown. `!` (history expansion, active outside quotes and inside double quotes) is always
/// escaped by closing the open quote around a backslash-escaped `!` and reopening it.
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

/// A script this does not recognise is returned as is: only plain candidates are then offered.
fn with_line_passed(script: &str) -> String {
    script.replace(
        ANCHOR,
        &format!("{ANCHOR}        {LINE_VAR}=\"${{COMP_LINE:0:COMP_POINT}}\" \\\n"),
    )
}

#[derive(Debug, Clone, Copy)]
pub(super) struct FirectlBash;

impl EnvCompleter for FirectlBash {
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
        assert_eq!(after("firectl query users"), Cursor::Bare);
        assert_eq!(after("firectl --where 'age"), Cursor::Single);
        assert_eq!(after("firectl --where 'age == 1' --sel"), Cursor::Bare);
        assert_eq!(after(r#"firectl --where "a"#), Cursor::Double);
        assert_eq!(after(r#"firectl --where "a \" b"#), Cursor::Double);
        assert_eq!(after(r"firectl a\'b"), Cursor::Bare);
        assert_eq!(after(r#"firectl 'it"s"#), Cursor::Single);
        assert_eq!(after(r#"firectl "it's""#), Cursor::Bare);
        assert_eq!(after(r"firectl 'a\"), Cursor::Single);
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
        assert_eq!(
            bash_words(candidates, Some("firectl query users --select ")),
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

    fn evaluated(prefix: &str, word: &str, suffix: &str) -> String {
        let out = std::process::Command::new("bash")
            .args(["-c", &format!("printf %s {prefix}{word}{suffix}")])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

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
        FirectlBash
            .write_registration(
                "FIRECTL_COMPLETE",
                "firectl",
                "firectl",
                "/bin/firectl",
                &mut script,
            )
            .unwrap();
        let script = String::from_utf8(script).unwrap();
        assert!(script.contains("_FIRECTL_COMP_LINE=\"${COMP_LINE:0:COMP_POINT}\""));
        assert!(script.contains("FIRECTL_COMPLETE=\"bash\""));
        assert!(script.contains("_CLAP_COMPLETE_INDEX"));
    }

    #[test]
    fn an_unrecognised_script_is_left_as_it_is() {
        let other = "_other() { :; }\ncomplete -F _other firectl\n";
        assert_eq!(with_line_passed(other), other);
        assert!(!with_line_passed(other).contains(LINE_VAR));
    }
}
