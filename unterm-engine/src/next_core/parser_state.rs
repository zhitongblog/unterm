#[derive(Default)]
pub(super) enum ParserState {
    #[default]
    Ground,
    Escape,
    EscapeIgnoreOne,
    EscapeHash,
    Csi(String),
    Osc(String),
    OscEscape(String),
    IgnoredString,
    IgnoredStringEscape,
    /// `DCS … ST`, kept for sixel.
    Dcs(String),
    DcsEscape(String),
    /// `APC … ST`, kept for the kitty graphics protocol.
    Apc(String),
    ApcEscape(String),
}
