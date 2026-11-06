const WORD_BLOCKLIST: &[&str] = &[
    "eval", "exec", "source", ".", "sh", "bash", "zsh", "dash", "ash", "ksh", "fish", "csh",
    "tcsh", "env", "xargs", "sudo", "doas", "su", "nohup", "time", "command", "builtin", "timeout",
    "nice", "ionice", "setsid", "stdbuf", "if", "then", "elif", "else", "fi", "for", "while",
    "until", "do", "done", "case", "esac", "in", "function", "select", "coproc", "!",
];

const REDIRECTS: &[&str] = &["2>&1", "2>/dev/null"];

pub fn split(command: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = command.trim().chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c.is_control() && c != '\n' && c != '\t' {
            return None;
        }
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                }
                current.push(c);
            }
            Some(_) => {
                match c {
                    '"' => quote = None,
                    '\\' | '`' => return None,
                    '$' if matches!(next, Some('(' | '{')) => return None,
                    _ => {}
                }
                current.push(c);
            }
            None => {
                if c == '2'
                    && at_word_start(&current)
                    && let Some(len) = redirect_len(&chars[i..])
                {
                    i += len;
                    if current.ends_with([' ', '\t']) {
                        while chars.get(i).is_some_and(|c| matches!(c, ' ' | '\t')) {
                            i += 1;
                        }
                    }
                    continue;
                }
                match c {
                    '\'' | '"' => {
                        quote = Some(c);
                        current.push(c);
                    }
                    '\n' | ';' => parts.push(take(&mut current)?),
                    '|' => {
                        if next == Some('|') {
                            i += 1;
                        }
                        parts.push(take(&mut current)?);
                    }
                    '&' if next == Some('&') => {
                        i += 1;
                        parts.push(take(&mut current)?);
                    }
                    '&' | '\\' | '`' | '$' | '(' | ')' | '{' | '}' | '<' | '>' | '#' => {
                        return None;
                    }
                    c if c.is_whitespace() && !c.is_ascii() => return None,
                    c => current.push(c),
                }
            }
        }
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    parts.push(take(&mut current)?);
    Some(parts)
}

fn at_word_start(current: &str) -> bool {
    current.is_empty() || current.ends_with([' ', '\t'])
}

fn redirect_len(rest: &[char]) -> Option<usize> {
    REDIRECTS.iter().find_map(|token| {
        let len = token.chars().count();
        let matches = rest.iter().take(len).copied().eq(token.chars());
        let bounded = rest
            .get(len)
            .is_none_or(|c| matches!(c, ' ' | '\t' | '\n' | ';' | '|' | '&'));
        (matches && bounded).then_some(len)
    })
}

fn take(current: &mut String) -> Option<String> {
    let part = std::mem::take(current).trim().to_string();
    if part.is_empty() || !runs_plain_command(&part) {
        return None;
    }
    Some(part)
}

fn runs_plain_command(part: &str) -> bool {
    let word = first_word(part);
    if word.contains('=') {
        return false;
    }
    let bare: String = word.chars().filter(|c| !matches!(c, '\'' | '"')).collect();
    let name = bare.rsplit('/').next().unwrap_or(&bare);
    !WORD_BLOCKLIST.contains(&name)
}

fn first_word(part: &str) -> &str {
    let mut quote: Option<char> = None;
    for (at, c) in part.char_indices() {
        match (quote, c) {
            (None, ' ' | '\t') => return &part[..at],
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            _ => {}
        }
    }
    part
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn some(parts: &[&str]) -> Option<Vec<String>> {
        Some(parts.iter().map(|p| p.to_string()).collect())
    }

    #[test]
    fn plain_and_compound_commands_split_into_their_parts() {
        let table: &[(&str, &[&str])] = &[
            ("ls", &["ls"]),
            ("  ls -la  ", &["ls -la"]),
            ("ls\n", &["ls"]),
            ("git status && ls", &["git status", "ls"]),
            ("ls; rm x", &["ls", "rm x"]),
            ("a && b || c", &["a", "b", "c"]),
            ("a | b", &["a", "b"]),
            ("a | b | c", &["a", "b", "c"]),
            ("a\nb", &["a", "b"]),
            ("a;b", &["a", "b"]),
            ("a&&b", &["a", "b"]),
            ("a||b", &["a", "b"]),
            ("a; a", &["a", "a"]),
            ("echo \"a;b\"", &["echo \"a;b\""]),
            ("echo 'a|b'", &["echo 'a|b'"]),
            ("echo \"a && b\" && ls", &["echo \"a && b\"", "ls"]),
            ("echo 'a\nb'", &["echo 'a\nb'"]),
            ("echo \"a\nb\"; ls", &["echo \"a\nb\"", "ls"]),
            (
                "git commit -m \"feat (x): y > z # 1\"",
                &["git commit -m \"feat (x): y > z # 1\""],
            ),
            ("echo '$(x) `y` ${z} \\'", &["echo '$(x) `y` ${z} \\'"]),
            ("echo \"$HOME\"", &["echo \"$HOME\""]),
            ("a 2>&1", &["a"]),
            ("a 2>/dev/null", &["a"]),
            ("a 2>&1 | b", &["a", "b"]),
            ("a 2>&1; b", &["a", "b"]),
            ("a 2>&1&&b", &["a", "b"]),
            ("a 2>&1 b", &["a b"]),
            ("a 2>/dev/null 2>&1", &["a"]),
            ("2>&1 a", &["a"]),
            ("./run.sh && /usr/bin/ls", &["./run.sh", "/usr/bin/ls"]),
            ("cargo test -- --nocapture", &["cargo test -- --nocapture"]),
            ("echo a=b", &["echo a=b"]),
        ];
        for (command, parts) in table {
            assert_eq!(split(command), some(parts), "{command:?}");
        }
    }

    #[test]
    fn anything_the_splitter_cannot_vouch_for_is_none() {
        let table = [
            "",
            "   ",
            "\n",
            "$(x)",
            "`x`",
            "\"$(x)\"",
            "\"`x`\"",
            "\"${x}\"",
            "ls ${HOME}",
            "ls $HOME",
            "ls $1",
            "ls $'x'",
            "ls $\"x\"",
            "a & b",
            "a &",
            "a &> f",
            "a |& b",
            "a > f",
            "a >> f",
            "a >| f",
            "a 2> f",
            "a 2>&1x",
            "a 2>&12",
            "a2>&1",
            "a 1>&2",
            "a &>/dev/null",
            "a 2>/dev/nullx",
            "a < f",
            "a <(b)",
            "a >(b)",
            "cat <<EOF",
            "cat <<-EOF",
            "cat <<< x",
            "(a)",
            "a (b)",
            "{ a; }",
            "a }",
            "echo {a,b}",
            "FOO=1 a",
            "FOO=\"a b\" a",
            "A=1 B=2 a",
            "a && FOO=1 b",
            "eval a",
            "exec a",
            "source a",
            ". a",
            "sh -c x",
            "bash -c x",
            "/bin/bash -c x",
            "\"bash\" -c x",
            "'sh' -c x",
            "zsh x",
            "dash x",
            "env a",
            "/usr/bin/env a",
            "xargs rm",
            "ls | xargs rm",
            "sudo a",
            "nohup a",
            "time a",
            "timeout 5 a",
            "nice a",
            "command a",
            "ls && sudo a",
            "ls | sh",
            "echo 'unclosed",
            "echo \"unclosed",
            "echo 'a' \"b",
            "a;",
            "a &&",
            "a ||",
            "a |",
            "; a",
            "&& a",
            "a;;b",
            "a ; ; b",
            "a\n\nb",
            "a # b",
            "a #b",
            "#a",
            "a#b",
            "a \\",
            "a \\\n b",
            "a\\;b",
            "echo \"a\\\"b\"",
            "echo \"a\\\\\"",
            "a\rb",
            "a\u{0}b",
            "a\u{7}b",
            "a\u{a0}b",
            "a\u{2003}b",
            "if a; then b; fi",
            "for i in a; do b; done",
            "while a; do b; done",
            "case a in b) c;; esac",
            "! a",
            "function f { a; }",
            "a () { b; }",
            "a && (b)",
            "ls `pwd`",
            "ls $(pwd)",
            "echo hi; $(rm x)",
            "ls && $(cat x)",
        ];
        for command in table {
            assert_eq!(split(command), None, "{command:?}");
        }
    }

    #[test]
    fn a_part_that_is_unsafe_makes_the_whole_command_none() {
        assert_eq!(split("ls && ls && bash x"), None);
        assert_eq!(split("ls && ls && ls"), some(&["ls", "ls", "ls"]));
    }

    fn outside_quotes(part: &str) -> String {
        let mut quote: Option<char> = None;
        let mut out = String::new();
        for c in part.chars() {
            match (quote, c) {
                (None, '\'' | '"') => quote = Some(c),
                (Some(q), _) if c == q => quote = None,
                (None, _) => out.push(c),
                _ => {}
            }
        }
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        #[test]
        fn split_never_panics_and_parts_have_no_separators(
            command in prop::collection::vec(
                prop::sample::select(vec![
                    "a", "ls", "git", " ", " ", "\t", "\n", ";", "&", "|", "&&", "||", "'", "\"",
                    "$", "(", ")", "{", "}", "<", ">", "#", "\\", "`", "$(", "${", "2>&1",
                    "2>/dev/null", "=", "-x", "/", "bash", "é", "\u{a0}", "\r",
                ]),
                0..14,
            ).prop_map(|v| v.concat())
        ) {
            if let Some(parts) = split(&command) {
                prop_assert!(!parts.is_empty());
                for part in &parts {
                    prop_assert!(!part.trim().is_empty());
                    let bare = outside_quotes(part);
                    for forbidden in [';', '&', '|', '\n', '<', '>', '`', '$', '\\', '(', ')', '{', '}', '#'] {
                        prop_assert!(!bare.contains(forbidden), "{part:?} in {command:?}");
                    }
                    prop_assert_eq!(split(part), Some(vec![part.clone()]), "{:?}", part);
                }
            }
        }

        #[test]
        fn arbitrary_text_never_panics(command in "\\PC{0,40}") {
            let _ = split(&command);
        }

        #[test]
        fn joining_safe_parts_gives_the_same_parts_back(
            parts in prop::collection::vec("x[a-z]{0,4}( -[a-z]| '[a-z ;|&]{0,4}')?", 1..5),
            sep in prop::sample::select(vec![" && ", " || ", " ; ", " | ", "\n", ";"]),
        ) {
            prop_assert_eq!(split(&parts.join(sep)), Some(parts));
        }
    }
}
