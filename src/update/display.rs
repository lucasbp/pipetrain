use std::path::Path;

pub fn readable_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if has_drive(rest) => rest.to_string(),
        _ => text,
    }
}

fn has_drive(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(text: &str) -> String {
        readable_path(Path::new(text))
    }

    #[test]
    fn a_verbatim_drive_path_loses_the_prefix() {
        assert_eq!(shown(r"\\?\C:\x"), r"C:\x");
        assert_eq!(
            shown(r"\\?\D:\Portable\pipetrain\pipetrain.exe"),
            r"D:\Portable\pipetrain\pipetrain.exe"
        );
    }

    #[test]
    fn a_verbatim_unc_path_becomes_a_plain_unc_path() {
        assert_eq!(shown(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
    }

    #[test]
    fn other_paths_are_left_alone() {
        assert_eq!(shown(r"C:\x\pipetrain.exe"), r"C:\x\pipetrain.exe");
        assert_eq!(shown(r"\\srv\share\x"), r"\\srv\share\x");
        assert_eq!(
            shown("/usr/local/bin/pipetrain"),
            "/usr/local/bin/pipetrain"
        );
        assert_eq!(shown(r"\\?\Volume{1234}\x"), r"\\?\Volume{1234}\x");
    }
}
