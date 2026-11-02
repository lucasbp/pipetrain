use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn lookup(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, rest) = line.trim().split_once(char::is_whitespace)?;
        let file = rest.trim().trim_start_matches('*');
        let valid = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
        (valid && file == name).then(|| hash.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn hashes_known_input() {
        assert_eq!(sha256_hex(b""), EMPTY);
    }

    #[test]
    fn finds_a_file_in_text_and_binary_mode_lines() {
        let sums = format!("{EMPTY}  a.zip\n{}  *b.tar.gz\n", EMPTY.to_uppercase());
        assert_eq!(lookup(&sums, "a.zip").as_deref(), Some(EMPTY));
        assert_eq!(lookup(&sums, "b.tar.gz").as_deref(), Some(EMPTY));
        assert_eq!(lookup(&sums, "c.zip"), None);
    }

    #[test]
    fn ignores_malformed_lines_and_partial_names() {
        let sums = format!("nothex  a.zip\n{EMPTY}  xa.zip\n{EMPTY}\n");
        assert_eq!(lookup(&sums, "a.zip"), None);
    }
}
