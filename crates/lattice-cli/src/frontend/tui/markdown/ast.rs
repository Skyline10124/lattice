pub(super) enum MdBlock {
    Heading {
        level: u8,
        spans: Vec<MdSpan>,
    },
    Paragraph {
        spans: Vec<MdSpan>,
    },
    CodeBlock {
        lang: Option<String>,
        text: String,
    },
    Blockquote {
        spans: Vec<MdSpan>,
    },
    ListItem {
        depth: u32,
        marker: ListMarker,
        spans: Vec<MdSpan>,
    },
    Table {
        headers: Vec<Vec<MdSpan>>,
        rows: Vec<Vec<Vec<MdSpan>>>,
    },
    Math {
        _display: bool,
        text: String,
    },
    Rule,
}

pub(super) enum ListMarker {
    Bullet,
    Numbered(u64),
    Task { checked: bool },
}

pub(super) struct MdSpan {
    pub(super) text: String,
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) strikethrough: bool,
    pub(super) code: bool,
    pub(super) link_url: Option<String>,
}
