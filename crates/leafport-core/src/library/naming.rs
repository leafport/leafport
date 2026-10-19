//! Title folder naming, compatible with the folders Adobe Digital Editions creates.

const MAX_NAME_BYTES: usize = 200;
const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

fn is_dropped(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        || ('\u{0}'..='\u{1f}').contains(&c)
        || c == '\u{7f}'
}

fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return true;
    }
    let b = stem.as_bytes();
    b.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && (b'1'..=b'9').contains(&b[3])
}

/// Turns a title into a folder name that is valid on every platform.
pub fn folder_name(title: &str) -> String {
    let filtered: String = title.chars().filter(|c| !is_dropped(*c)).collect();
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    let is_edge = |c: char| c == '.' || c == ' ';
    let mut name = collapsed
        .trim_start_matches(is_edge)
        .trim_end_matches(is_edge)
        .to_owned();
    if name.len() > MAX_NAME_BYTES {
        let mut end = MAX_NAME_BYTES;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
        name = name.trim_end_matches(is_edge).to_owned();
    }
    if name.is_empty() {
        return "Untitled".to_owned();
    }
    if is_reserved(&name) {
        name.push('_');
    }
    name
}

/// Suffix that tells apart folders for different titles with the same name.
pub fn clash_suffix(resource: &str) -> String {
    let id = resource.strip_prefix("urn:uuid:").unwrap_or(resource);
    let head: String = id.chars().take(8).filter(|c| !is_dropped(*c)).collect();
    format!(" [{head}]")
}
