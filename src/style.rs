pub(crate) fn paint(code: &str, text: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

pub(crate) fn dim(text: &str) -> String {
    paint("2", text)
}

pub(crate) fn bold(text: &str) -> String {
    paint("1", text)
}

pub(crate) fn red(text: &str) -> String {
    paint("1;31", text)
}

pub(crate) fn green(text: &str) -> String {
    paint("32", text)
}

pub(crate) fn yellow(text: &str) -> String {
    paint("33", text)
}
