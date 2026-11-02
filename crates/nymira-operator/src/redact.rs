//! Secret scrubbing for text that leaves the process (alarm messages, error text).
//!
//! Provider keys start with `sk-`, reporting tokens with `rpt-`, and an `Authorization` header
//! or a `Bearer` credential carries a secret after its name. All of them become `[redacted]`.

const MASK: &str = "[redacted]";

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '=' | '+' | '/')
}

/// `text` with every provider key, reporting token, `Bearer` credential and `Authorization`
/// value replaced by `[redacted]`.
pub fn scrub(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let rest = &lower[i..];
        let skip = if boundary && (rest.starts_with("sk-") || rest.starts_with("rpt-")) {
            Some(span(text, i, is_token_char))
        } else if boundary && rest.starts_with("bearer ") {
            Some(span(text, i + 7, is_token_char))
        } else if boundary && rest.starts_with("authorization") {
            // The rest of the line is the value, whatever its scheme.
            let after = i + "authorization".len();
            let tail = &text[after..];
            let trimmed = tail.trim_start_matches([' ', '"', '\'']);
            if trimmed.starts_with([':', '=']) {
                Some(span(text, after, |c| c != '\n' && c != '\r'))
            } else {
                None
            }
        } else {
            None
        };
        match skip {
            Some(end) => {
                out.push_str(MASK);
                i = end;
            }
            None => {
                let ch = text[i..].chars().next().expect("a char starts here");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// The end of the run of characters matching `keep`, starting at `from`.
fn span(text: &str, from: usize, keep: impl Fn(char) -> bool) -> usize {
    text[from..]
        .char_indices()
        .find(|(_, c)| !keep(*c))
        .map_or(text.len(), |(n, _)| from + n)
}

#[cfg(test)]
mod tests {
    use super::scrub;

    #[test]
    fn keys_tokens_and_authorization_values_go() {
        for (input, gone) in [
            ("key sk-live-abcDEF123 failed", "sk-live-abcDEF123"),
            ("token rpt-9f8e7d6c5b4a used", "rpt-9f8e7d6c5b4a"),
            ("sent Bearer eyJhbGciOi.payload.sig now", "eyJhbGciOi"),
            ("Authorization: Basic dXNlcjpwYXNz\nnext", "dXNlcjpwYXNz"),
            ("authorization=Bearer abc123xyz", "abc123xyz"),
            ("SK-UPPER1234567 here", "SK-UPPER1234567"),
        ] {
            let out = scrub(input);
            assert!(!out.contains(gone), "{input} -> {out}");
            assert!(out.contains("[redacted]"), "{input} -> {out}");
        }
    }

    #[test]
    fn ordinary_text_is_untouched() {
        for text in [
            "balance low: 2.10 USD",
            "ask-me about task-force, risk-free and desk-lamp",
            "héllo wörld — ünïcode",
            "the authorization of the vault owner",
        ] {
            assert_eq!(scrub(text), text);
        }
    }
}
