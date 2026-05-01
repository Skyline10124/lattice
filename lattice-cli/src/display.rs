use colored::Colorize;

/// Checkmark or cross icon for credential/auth status display.
pub fn status_icon(ok: bool) -> &'static str {
    if ok {
        "\u{2713}"
    } else {
        "\u{2717}"
    }
}

/// Colored credential status label ("set" / "missing") for auth display.
pub fn credential_label(ok: bool) -> colored::ColoredString {
    if ok {
        "set".green()
    } else {
        "missing".red()
    }
}
