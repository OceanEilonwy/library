//! Small text helpers shared across modules.

/// Lower-cases, strips diacritics-ish punctuation and collapses whitespace so
/// that "The Hobbit: or, There and Back Again" and "the hobbit" compare well.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for ch in s.chars().flat_map(char::to_lowercase) {
        let ch = fold_diacritic(ch);
        if ch.is_alphanumeric() {
            out.push(ch);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Normalized title with a leading article removed.
pub fn title_key(s: &str) -> String {
    let n = normalize(s);
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = n.strip_prefix(article) {
            return rest.to_owned();
        }
    }
    n
}

fn fold_diacritic(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'è' | 'é' | 'ê' | 'ë' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ł' => 'l',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
        'ś' | 'š' => 's',
        'ù' | 'ú' | 'û' | 'ü' | 'ů' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

/// Keeps only the digits (and a trailing X for ISBN-10) of an identifier.
pub fn normalize_identifier(s: &str) -> String {
    let mut out: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'x' || *c == 'X')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    // An X is only meaningful as the ISBN-10 check digit.
    if let Some(pos) = out.find('X')
        && (pos != out.len() - 1 || out.len() != 10)
    {
        out.retain(|c| c != 'X');
    }
    out
}

/// True if `s` looks like an ISBN-10/13 or an EAN/UPC barcode.
pub fn looks_like_code(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.chars().any(|c| c.is_alphabetic() && c != 'x' && c != 'X') {
        return false;
    }
    matches!(normalize_identifier(trimmed).len(), 8 | 10 | 12 | 13)
}

/// Converts an ISBN-10 to ISBN-13 so both forms compare equal.
pub fn isbn13(code: &str) -> String {
    let code = normalize_identifier(code);
    if code.len() != 10 {
        return code;
    }
    let core = format!("978{}", &code[..9]);
    let sum: u32 = core
        .chars()
        .enumerate()
        .map(|(i, c)| c.to_digit(10).unwrap_or(0) * if i % 2 == 0 { 1 } else { 3 })
        .sum();
    format!("{core}{}", (10 - sum % 10) % 10)
}

/// Normalized Levenshtein similarity in 0..=1.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let dist = prev[b.len()];
    1.0 - dist as f64 / a.len().max(b.len()) as f64
}

/// Turns a name into a URL slug.
pub fn slugify(s: &str) -> String {
    let n = normalize(s);
    n.replace(' ', "-")
}

/// Trims and caps a user-supplied string to `max` characters.
pub fn clean(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_case_punctuation_and_diacritics() {
        assert_eq!(normalize("  The Hobbit: or, There & Back! "), "the hobbit or there back");
        assert_eq!(normalize("Agnieszka Nowak-Łęcka"), "agnieszka nowak lecka");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn title_key_drops_articles() {
        assert_eq!(title_key("The Hobbit"), "hobbit");
        assert_eq!(title_key("A Wizard of Earthsea"), "wizard of earthsea");
        assert_eq!(title_key("Theory"), "theory");
    }

    #[test]
    fn identifiers() {
        assert_eq!(normalize_identifier("978-0-261-10295-4"), "9780261102954");
        assert_eq!(normalize_identifier("0-261-10295-x"), "026110295X");
        assert_eq!(normalize_identifier("X123"), "123");
        assert!(looks_like_code("978-0-261-10295-4"));
        assert!(looks_like_code("0261102958"));
        assert!(!looks_like_code("the hobbit"));
        assert!(!looks_like_code("123"));
        assert_eq!(isbn13("0-261-10295-8"), "9780261102958");
        assert_eq!(isbn13("0140449132"), "9780140449136");
        assert_eq!(isbn13("9780261102954"), "9780261102954");
    }

    #[test]
    fn similarity_bounds() {
        assert_eq!(similarity("abc", "abc"), 1.0);
        assert_eq!(similarity("", ""), 1.0);
        assert_eq!(similarity("abc", "xyz"), 0.0);
        assert!(similarity("tolkien", "tolkein") > 0.7);
    }

    #[test]
    fn slugs_and_clean() {
        assert_eq!(slugify("Equipment · Sports"), "equipment-sports");
        assert_eq!(clean("  hello world ", 5), "hello");
    }
}
