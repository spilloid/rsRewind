//! User text -> safe FTS5 MATCH expression.
//!
//! The only FTS5 syntax we ever emit is: double-quoted strings, an optional trailing `*` (prefix),
//! and whitespace (implicit AND). Everything the user typed ends up *inside* a quoted string with
//! `"` doubled, so `NEAR(`, `col:`, `AND`/`OR`/`NOT`, `-`, `^`, `+`, parentheses and braces are
//! all literal text and can never become operators or column filters.

/// A MATCH string built by [`parse_query`]. `None` inside means "no full-text condition".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FtsExpr(Option<String>);

impl FtsExpr {
    /// The expression to bind to `ocr_fts MATCH ?`, or `None` when the text had no searchable
    /// words (empty, whitespace, punctuation only).
    pub fn as_match(&self) -> Option<&str> {
        self.0.as_deref()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }
}

/// Turns arbitrary user text into a safe FTS5 expression.
///
/// - bare words become quoted phrases: `budget` -> `"budget"`;
/// - `"exact phrase"` stays one phrase (an unterminated quote runs to the end of the input);
/// - a trailing `*` keeps prefix search: `quart*` -> `"quart"*`, `"foo ba"*` -> `"foo ba"*`;
/// - a `"` inside a word is literal and doubled: `a"b` -> `"a""b"`;
/// - terms without a single letter or digit are dropped: they tokenize to nothing, and an empty
///   phrase would make the implicit AND match nothing at all.
pub fn parse_query(input: &str) -> FtsExpr {
    // Control characters are separators. NUL in particular must never reach FTS5: its expression
    // parser stops at NUL and reports an unterminated string.
    let chars: Vec<char> = input
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut terms: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '"' {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && chars[end] != '"' {
                end += 1;
            }
            let content: String = chars[start..end].iter().collect();
            // Skip the closing quote if there is one.
            i = if end < chars.len() { end + 1 } else { end };
            let prefix = end < chars.len() && chars.get(i) == Some(&'*');
            if prefix {
                while chars.get(i) == Some(&'*') {
                    i += 1;
                }
            }
            push_term(&mut terms, &content, prefix);
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            let stripped = token.trim_end_matches('*');
            let prefix = stripped.len() != token.len();
            push_term(&mut terms, stripped, prefix);
        }
    }
    if terms.is_empty() {
        FtsExpr(None)
    } else {
        FtsExpr(Some(terms.join(" ")))
    }
}

fn push_term(terms: &mut Vec<String>, text: &str, prefix: bool) {
    // unicode61 keeps letters and numbers and splits on everything else.
    if !text.chars().any(char::is_alphanumeric) {
        return;
    }
    let mut term = String::with_capacity(text.len() + 3);
    term.push('"');
    term.push_str(&text.replace('"', "\"\""));
    term.push('"');
    if prefix {
        term.push('*');
    }
    terms.push(term);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn expected_translations() {
        let cases: &[(&str, Option<&str>)] = &[
            ("", None),
            ("   \t\n ", None),
            ("hello", Some("\"hello\"")),
            ("hello   world", Some("\"hello\" \"world\"")),
            ("\"exact phrase\"", Some("\"exact phrase\"")),
            (
                "before \"exact phrase\" after",
                Some("\"before\" \"exact phrase\" \"after\""),
            ),
            ("\"unterminated phrase", Some("\"unterminated phrase\"")),
            ("quart*", Some("\"quart\"*")),
            ("quart***", Some("\"quart\"*")),
            ("\"foo ba\"*", Some("\"foo ba\"*")),
            ("*foo", Some("\"*foo\"")),
            ("a\"b", Some("\"a\"\"b\"")),
            ("say\"\"hi", Some("\"say\"\"\"\"hi\"")),
            ("NEAR(a b)", Some("\"NEAR(a\" \"b)\"")),
            ("NEAR/2", Some("\"NEAR/2\"")),
            ("text:secret", Some("\"text:secret\"")),
            ("{text}: x", Some("\"{text}:\" \"x\"")),
            ("-foo", Some("\"-foo\"")),
            ("foo -bar", Some("\"foo\" \"-bar\"")),
            ("^start", Some("\"^start\"")),
            ("AND OR NOT", Some("\"AND\" \"OR\" \"NOT\"")),
            ("a + b", Some("\"a\" \"b\"")),
            ("(a OR b)", Some("\"(a\" \"OR\" \"b)\"")),
            ("café", Some("\"café\"")),
            ("東京 タワー", Some("\"東京\" \"タワー\"")),
            // Punctuation-only input has nothing to search for.
            ("- ( ) * : ^ + \" \"\" , .", None),
            ("\"\"", None),
            ("\"", None),
            ("🙂", None),
            ("a\0b", Some("\"a\" \"b\"")),
            ("\"a\0b\"", Some("\"a b\"")),
            ("x\u{7}y", Some("\"x\" \"y\"")),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_query(input).as_match(), *expected, "input: {input:?}");
        }
    }

    /// Hostile inputs: none may produce an FTS5 syntax error, and each must match literally.
    const HOSTILE: &[&str] = &[
        "NEAR(",
        "NEAR(a b",
        "NEAR(a b, 2)",
        "a NEAR b",
        "NEAR/3 x",
        "col:",
        "text:",
        "text:secret",
        "rowid:1",
        "ocr_fts:x",
        "{text}:x",
        "- text : x",
        "\"",
        "\"\"",
        "\"\"\"",
        "\"a\"\"b\"",
        "a\"b\"c",
        "\"a\" \"b\"*",
        "-",
        "-foo",
        "foo -bar",
        "^",
        "^start",
        "AND",
        "OR",
        "NOT",
        "AND OR NOT",
        "a AND",
        "OR b",
        "NOT NOT NOT",
        "(",
        ")",
        "(a OR b)",
        "((((",
        "*",
        "**",
        "*a",
        "a**",
        "a*b",
        "+",
        "a + b",
        "'; DROP TABLE ocr_fts; --",
        "x' OR '1'='1",
        "\0",
        "a\0b",
        "[a]",
        "a,b",
        "e\u{301}",
        "ＡＮＤ",
        "\u{202e}evil",
        "שלום",
        "🙂 smile",
        "\t\r\n",
    ];

    #[test]
    fn hostile_inputs_never_reach_fts5_as_syntax() -> TestResult {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE t USING fts5 (text, tokenize = 'unicode61 remove_diacritics 2');
             INSERT INTO t (rowid, text) VALUES
                 (1, 'near and or not text secret rowid col'),
                 (2, 'start foo bar drop table ocr fts'),
                 (3, 'unrelated content');",
        )?;
        for input in HOSTILE {
            let expr = parse_query(input);
            let Some(m) = expr.as_match() else { continue };
            let result: rusqlite::Result<i64> =
                conn.query_row("SELECT COUNT(*) FROM t WHERE t MATCH ?1", [m], |row| {
                    row.get(0)
                });
            assert!(result.is_ok(), "input {input:?} -> {m:?} -> {result:?}");
        }
        // The same strings passed raw *do* break FTS5, which is what makes the escaping matter.
        let mut raw_errors = 0;
        for input in ["NEAR(", "col:", "\"", "AND", "(", "-foo", "^", "*"] {
            let raw: rusqlite::Result<i64> =
                conn.query_row("SELECT COUNT(*) FROM t WHERE t MATCH ?1", [input], |row| {
                    row.get(0)
                });
            raw_errors += usize::from(raw.is_err());
        }
        assert!(raw_errors >= 6, "only {raw_errors} raw inputs failed");
        Ok(())
    }

    #[test]
    fn operators_are_matched_as_words() -> TestResult {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE t USING fts5 (text, tokenize = 'unicode61 remove_diacritics 2');
             INSERT INTO t (rowid, text) VALUES
                 (1, 'Please read the text: secret plans'),
                 (2, 'cats AND dogs'),
                 (3, 'cats or dogs'),
                 (4, 'quarterly report');",
        )?;
        let ids = |input: &str| -> rusqlite::Result<Vec<i64>> {
            let Some(m) = parse_query(input).as_match().map(str::to_owned) else {
                return Ok(Vec::new());
            };
            let mut stmt = conn.prepare("SELECT rowid FROM t WHERE t MATCH ?1 ORDER BY rowid")?;
            stmt.query_map([m], |row| row.get(0))?.collect()
        };
        // `text:secret` is the two words "text secret" in sequence, not a column filter.
        assert_eq!(ids("text:secret")?, vec![1]);
        // `AND` is a word that must be present, not an operator.
        assert_eq!(ids("cats AND dogs")?, vec![2]);
        assert_eq!(ids("cats OR dogs")?, vec![3]);
        // `-dogs` does not exclude dogs.
        assert_eq!(ids("cats -dogs")?, vec![2, 3]);
        assert_eq!(ids("quart*")?, vec![4]);
        assert_eq!(ids("\"report quarterly\"")?, Vec::<i64>::new());
        assert_eq!(ids("\"quarterly report\"")?, vec![4]);
        Ok(())
    }
}
