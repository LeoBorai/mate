//! Slash commands (§10-shaped, `M13-3`): `/new`, `/close`, `/rename`, `/model`, `/provider`,
//! `/tools`, `/http`, `/clear`, `/tokens`, `/quit`. [`parse`] is the whole surface — a pure
//! function from the submitted input line to a typed [`SlashCommand`], with no side effects and
//! no knowledge of `App` at all, so `crate::app::App::on_key` can decide *before* anything is
//! dispatched whether the line the user just submitted is a command or a prompt. An unparseable
//! `/foo` still parses, to [`SlashCommand::Unknown`] — the point is that nothing starting with
//! `/` ever falls through to `SessionCmd::Prompt` and reaches the model.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlashCommand {
    New(Option<String>),
    Close,
    Rename(Option<String>),
    Model(Option<String>),
    Provider(Option<String>),
    Tools,
    Http(Option<String>),
    Clear,
    Tokens,
    Quit,
    Unknown(String),
}

/// `None` if `input` (already trimmed by the caller's input box) doesn't start with `/` — that's
/// an ordinary prompt, not a command, and the caller sends it on unchanged. Anything else always
/// parses to `Some`, worst case `Unknown`.
pub(crate) fn parse(input: &str) -> Option<SlashCommand> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }

    let mut parts = input[1..].splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_ascii_lowercase();
    let rest = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Some(match name.as_str() {
        "new" => SlashCommand::New(rest),
        "close" => SlashCommand::Close,
        "rename" => SlashCommand::Rename(rest),
        "model" => SlashCommand::Model(rest),
        "provider" => SlashCommand::Provider(rest),
        "tools" => SlashCommand::Tools,
        "http" => SlashCommand::Http(rest),
        "clear" => SlashCommand::Clear,
        "tokens" => SlashCommand::Tokens,
        "quit" | "q" => SlashCommand::Quit,
        _ => SlashCommand::Unknown(name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_prompt_does_not_parse_as_a_command() {
        assert_eq!(parse("what does this function do?"), None);
    }

    #[test]
    fn leading_whitespace_before_the_slash_still_parses() {
        assert_eq!(parse("  /quit"), Some(SlashCommand::Quit));
    }

    #[test]
    fn command_names_are_case_insensitive() {
        assert_eq!(parse("/QUIT"), Some(SlashCommand::Quit));
        assert_eq!(parse("/Clear"), Some(SlashCommand::Clear));
    }

    #[test]
    fn a_command_with_an_argument_captures_it_trimmed() {
        assert_eq!(
            parse("/rename   api server  "),
            Some(SlashCommand::Rename(Some("api server".to_string())))
        );
    }

    #[test]
    fn a_command_with_no_argument_carries_none_not_an_empty_string() {
        assert_eq!(parse("/rename"), Some(SlashCommand::Rename(None)));
        assert_eq!(parse("/rename   "), Some(SlashCommand::Rename(None)));
    }

    #[test]
    fn new_captures_an_optional_directory_argument() {
        assert_eq!(parse("/new"), Some(SlashCommand::New(None)));
        assert_eq!(
            parse("/new ../other-repo"),
            Some(SlashCommand::New(Some("../other-repo".to_string())))
        );
    }

    #[test]
    fn q_is_a_short_alias_for_quit() {
        assert_eq!(parse("/q"), Some(SlashCommand::Quit));
    }

    #[test]
    fn an_unrecognized_command_parses_to_unknown_rather_than_none() {
        assert_eq!(
            parse("/frobnicate"),
            Some(SlashCommand::Unknown("frobnicate".to_string())),
            "anything starting with / must never fall through to the model as a prompt"
        );
    }

    #[test]
    fn a_bare_slash_with_nothing_after_it_is_unknown_not_a_panic() {
        assert_eq!(parse("/"), Some(SlashCommand::Unknown(String::new())));
    }

    #[test]
    fn http_and_model_capture_their_optional_argument() {
        assert_eq!(parse("/http"), Some(SlashCommand::Http(None)));
        assert_eq!(
            parse("/http on"),
            Some(SlashCommand::Http(Some("on".to_string())))
        );
        assert_eq!(parse("/model"), Some(SlashCommand::Model(None)));
        assert_eq!(
            parse("/model org/other-model"),
            Some(SlashCommand::Model(Some("org/other-model".to_string())))
        );
    }
}
