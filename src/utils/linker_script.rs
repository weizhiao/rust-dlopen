use alloc::{string::String, vec::Vec};

/// A simple token-based parser for GNU linker scripts.
/// It recognizes GROUP, INPUT, and AS_NEEDED commands.
pub(crate) fn get_linker_script_libs(content: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(content);
    let mut libs = Vec::new();
    let tokens = tokenize(&s);

    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        if token.eq_ignore_ascii_case("GROUP") || token.eq_ignore_ascii_case("INPUT") {
            i += 1;
            if i < tokens.len() && tokens[i] == "(" {
                i += 1;
                i = parse_group(&tokens, i, &mut libs);
            }
        } else {
            i += 1;
        }
    }
    libs
}

fn tokenize(s: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut indices = s.char_indices().peekable();

    while let Some((start, c)) = indices.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '(' || c == ')' {
            tokens.push(&s[start..start + 1]);
            continue;
        }
        if c == '/' && indices.peek().map(|&(_, c2)| c2) == Some('*') {
            indices.next(); // '*'
            while let Some((_, cc)) = indices.next() {
                if cc == '*' && indices.peek().map(|&(_, c3)| c3) == Some('/') {
                    indices.next(); // '/'
                    break;
                }
            }
            continue;
        }

        let start_pos = start;
        let mut end_pos = start + c.len_utf8();
        while let Some(&(next_idx, next_c)) = indices.peek() {
            if next_c.is_whitespace() || next_c == '(' || next_c == ')' {
                break;
            }
            indices.next();
            end_pos = next_idx + next_c.len_utf8();
        }
        tokens.push(&s[start_pos..end_pos]);
    }
    tokens
}

fn parse_group(tokens: &[&str], mut i: usize, libs: &mut Vec<String>) -> usize {
    while i < tokens.len() {
        let token = tokens[i];
        match token {
            ")" => {
                i += 1;
                break;
            }
            "AS_NEEDED" => {
                i += 1;
                if i < tokens.len() && tokens[i] == "(" {
                    i += 1;
                    i = parse_group(tokens, i, libs);
                } else {
                    i += 1;
                }
            }
            _ => {
                if !token.starts_with('-') {
                    libs.push(alloc::string::String::from(token));
                }
                i += 1;
            }
        }
    }
    i
}
