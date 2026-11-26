use super::Pin;

pub(super) fn pins(text: &str, package: &str) -> Vec<Pin> {
    let mut pins = Vec::new();
    let mut overridden = false;
    let mut block = "";
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        let offset = line_start;
        line_start += line.len();
        let content = line.split("//").next().unwrap_or("").trim();
        if content == ")" {
            block = "";
            continue;
        }
        if let Some(directive) = content.strip_suffix('(') {
            block = directive.trim();
            continue;
        }
        let mut parts = content.split_ascii_whitespace();
        let directive = if block.is_empty() {
            parts.next().unwrap_or("")
        } else {
            block
        };
        let Some(path) = parts.next() else { continue };
        if path.trim_matches(['"', '`']) != package {
            continue;
        }
        match directive {
            "exclude" | "replace" => overridden = true,
            "require" => {
                let Some(version) = parts.next() else {
                    continue;
                };
                if parts.next().is_some() {
                    pins.push(Pin::manual("manual: unrecognized Go requirement"));
                    continue;
                }
                let start = offset + (version.as_ptr() as usize - line.as_ptr() as usize);
                pins.push(Pin {
                    redirected: false,
                    current: version.to_owned(),
                    span: Some(start..start + version.len()),
                });
            }
            _ => {}
        }
    }
    if overridden {
        for pin in &mut pins {
            *pin = Pin::alternate("manual: Go replace or exclude for this module");
        }
    }
    pins
}
