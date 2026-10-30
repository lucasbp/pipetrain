const CLI_URL: &str = "https://github.com/cli/cli#installation";
const NEW_TOKEN_URL: &str = "https://github.com/settings/personal-access-tokens/new";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhState {
    Missing,
    LoggedOut,
    LoggedIn,
}

fn install_command(os: Os) -> String {
    match os {
        Os::Windows => "winget install --id GitHub.cli".to_string(),
        Os::MacOs => "brew install gh".to_string(),
        Os::Linux => format!("see {CLI_URL}"),
    }
}

fn set_token_command(os: Os) -> &'static str {
    match os {
        Os::Windows => "$env:GH_TOKEN = \"<your-token>\"",
        Os::MacOs | Os::Linux => "export GH_TOKEN=<your-token>",
    }
}

fn token_alternative(os: Os) -> Vec<String> {
    vec![
        "Without gh: create a fine-grained token with read-only access to the repos or org you plan \
         (Pull requests: read, Contents: read, Metadata: read) at "
            .to_string()
            + NEW_TOKEN_URL,
        format!("then set it: {}", set_token_command(os)),
        "Keep the token out of files and shell history where possible. A classic token is not needed."
            .to_string(),
    ]
}

pub fn missing_token_fix(os: Os, gh: GhState) -> String {
    let mut lines = match gh {
        GhState::Missing => vec![
            format!("Install the GitHub CLI: {}", install_command(os)),
            "Then sign in: gh auth login".to_string(),
        ],
        GhState::LoggedOut | GhState::LoggedIn => vec!["Sign in: gh auth login".to_string()],
    };
    lines.extend(token_alternative(os));
    lines.join("\n")
}

pub fn short_no_token(os: Os, gh: GhState) -> String {
    let sign_in = match gh {
        GhState::Missing => format!(
            "install the GitHub CLI ({}) and run `gh auth login`",
            install_command(os)
        ),
        GhState::LoggedOut | GhState::LoggedIn => "run `gh auth login`".to_string(),
    };
    format!(
        "no GitHub token: {sign_in}, or set GH_TOKEN to a fine-grained read-only token; run `pipetrain doctor` for details"
    )
}

pub fn renew_token_fix() -> String {
    "Run `gh auth login` again, or create a new token and set GH_TOKEN. Keep a token out of files and shell history where possible.".to_string()
}

pub fn connectivity_fix() -> String {
    "Check your internet connection, VPN and proxy settings, then try again.".to_string()
}

pub fn sso_fix() -> String {
    "If the org uses SAML SSO, authorize the token for it in your GitHub token settings. This is a guess from the message GitHub sent, not a confirmed cause.".to_string()
}
