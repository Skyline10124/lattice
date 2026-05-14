/// Side effects produced by TUI state transitions.
///
/// The first rewrite keeps effect execution close to `App` while introducing
/// a typed vocabulary for the async boundaries that will keep shrinking.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TuiEffect {
    StartAgentTurn,
    RunBashCommand(String),
    SaveSession,
    CopyToClipboard(String),
    OpenPager(String),
}
