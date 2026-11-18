//! Just enough GraphQL lexing to tell queries from other operations.

/// Whether `document` has no mutation or subscription. These start with their
/// keyword outside any braces, parentheses and brackets, where otherwise only
/// `query`, `fragment` and names appear. A name that happens to be `mutation`
/// or `subscription` also counts, which errs on the safe side.
pub(super) fn only_queries(document: &str) -> bool {
    let mut depth = 0_i32;
    let mut rest = document;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix(r#"""""#) {
            rest = skip_block_string(after);
        } else if c == '"' {
            rest = skip_string(&rest[1..]);
        } else if c == '#' {
            rest = rest.find(['\n', '\r']).map_or("", |end| &rest[end..]);
        } else if matches!(c, '{' | '(' | '[') {
            depth += 1;
            rest = &rest[1..];
        } else if matches!(c, '}' | ')' | ']') {
            depth -= 1;
            rest = &rest[1..];
        } else if c == '_' || c.is_ascii_alphabetic() {
            let end = rest
                .find(|c: char| c != '_' && !c.is_ascii_alphanumeric())
                .unwrap_or(rest.len());
            if depth <= 0 && matches!(&rest[..end], "mutation" | "subscription") {
                return false;
            }
            rest = &rest[end..];
        } else {
            rest = &rest[c.len_utf8()..];
        }
    }
    true
}

/// Skips past the closing quote of a string whose opening quote is consumed.
fn skip_string(s: &str) -> &str {
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => return &s[i + 1..],
            _ => {}
        }
    }
    ""
}

/// Skips past the closing `"""` of a block string, where only `\"""` escapes.
fn skip_block_string(s: &str) -> &str {
    let mut from = 0;
    while let Some(i) = s[from..].find(r#"""""#).map(|i| from + i) {
        if !s[..i].ends_with('\\') {
            return &s[i + 3..];
        }
        from = i + 3;
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::only_queries;

    #[test]
    fn accepts_queries() {
        assert!(only_queries("{ account { id } }"));
        assert!(only_queries(
            "query A($id: ID!) { account(id: $id) { ...F mutation: id } }
             fragment F on Account { id }"
        ));
        assert!(only_queries(
            r#"# mutation in a comment
               query A { x(a: "mutation {", b: """ \""" mutation """) }"#
        ));
    }

    #[test]
    fn rejects_mutations_and_subscriptions() {
        assert!(!only_queries("mutation M { buy }"));
        assert!(!only_queries("query A { x } mutation B { buy }"));
        assert!(!only_queries("subscription S { ticks }"));
        assert!(!only_queries(
            r#"query A { x(a: "\"") } mutation B { buy }"#
        ));
        assert!(!only_queries(
            r#"query A { x(a: """\"""""") } mutation B { y }"#
        ));
        assert!(!only_queries("# comment\nmutation M { buy }"));
    }
}
