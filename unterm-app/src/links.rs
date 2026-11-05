//! Links a program marked, and links a program merely printed.
//!
//! Two different things, and both are expected to work. A program can mark
//! text as a link explicitly with OSC 8 -- `ls --hyperlink`, `gcc`'s
//! diagnostics, anything that knows it is writing to a terminal. Far more
//! often it just prints a URL, and the user still expects to be able to click
//! it.
//!
//! The printed kind is found by rules: a regex, and a format string that turns
//! the matched text into a URI. The built-in set recognises URLs and email
//! addresses; a config can write its own under `hyperlink_rules`, which
//! replace the built-in set entirely -- exactly as they always did -- so a
//! line saying `issue #123` can open the tracker.
//!
//! Opening is deliberately behind a modifier. A terminal where a stray click
//! launches a browser is a terminal you cannot select text in, and a program
//! that prints a URL should not be able to make one click open it.

use regex::Regex;
use unterm_engine::next_core::config::{Config, Value};
use unterm_engine::StyledScreenLine;

/// Where a link is on screen and what it points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Row within the viewport.
    pub row: usize,
    /// Half-open column range.
    pub start: usize,
    pub end: usize,
    pub uri: String,
    /// Whether a rule the user wrote produced this URI. Their rule's format
    /// string is the user saying what to open, so it opens as written; every
    /// other link stays behind the scheme whitelist.
    pub user_rule: bool,
}

impl Link {
    pub fn covers(&self, row: usize, column: usize) -> bool {
        self.row == row && column >= self.start && column < self.end
    }
}

/// One way of turning printed text into a link: a regex, a format string, and
/// which capture to show as the link.
///
/// The format string builds the URI: each `$N` is replaced by capture `N` of
/// the match, so `$0` is the whole match and `mailto:$0` prefixes it.
/// Replacement runs from the highest capture down, so `$11` is never read as
/// `$1` followed by a `1`.
pub struct Rule {
    regex: Regex,
    format: String,
    /// Which capture the link covers on screen -- the URL inside
    /// `(https://...)`, not the parentheses around it.
    highlight: usize,
}

impl Rule {
    pub fn new(pattern: &str, format: &str, highlight: usize) -> Result<Rule, regex::Error> {
        Ok(Rule {
            regex: Regex::new(pattern)?,
            format: format.to_string(),
            highlight,
        })
    }

    /// The URI the format string builds from a match.
    fn expand(&self, captures: &regex::Captures) -> String {
        let mut uri = self.format.clone();
        // Highest numbered capture first, so `$11` is not eaten by `$1`.
        for n in (0..captures.len()).rev() {
            let text = captures.get(n).map(|c| c.as_str()).unwrap_or("");
            uri = uri.replace(&format!("${n}"), text);
        }
        uri
    }
}

/// The rules in force: the user's own, or the built-in set.
pub struct Rules {
    rules: Vec<Rule>,
    /// Whether the config wrote them. A user's rules open what their format
    /// yields; the built-in set stays behind the scheme whitelist.
    user_defined: bool,
}

impl Rules {
    /// The rules the config asks for, or the built-in set if it says nothing.
    ///
    /// Setting `hyperlink_rules` replaces the built-in set entirely -- the
    /// behaviour these rules have always had -- so an empty list is how a
    /// config turns printed-link detection off. A rule whose regex does not
    /// compile is skipped here; the schema check has already reported it with
    /// a line number.
    pub fn from_config(config: &Config) -> Rules {
        let Ok(Some(entries)) = config.list_of("hyperlink_rules") else {
            return Rules::built_in();
        };
        let mut rules = Vec::new();
        for entry in entries {
            let Value::List(parts) = entry else { continue };
            let (pattern, format, highlight) = match parts.as_slice() {
                [Value::Str(pattern), Value::Str(format)] => (pattern, format, 0),
                [Value::Str(pattern), Value::Str(format), Value::Int(highlight)] => {
                    (pattern, format, (*highlight).max(0) as usize)
                }
                _ => continue,
            };
            match Rule::new(pattern, format, highlight) {
                Ok(rule) => rules.push(rule),
                Err(err) => log::warn!("hyperlink rule `{pattern}` skipped: {err}"),
            }
        }
        Rules {
            rules,
            user_defined: true,
        }
    }

    /// The set every terminal starts with: URLs, bracketed URLs, and email
    /// addresses -- the same six rules 0.57 shipped as its defaults.
    pub fn built_in() -> Rules {
        let rules = [
            // A URL wrapped in punctuation: (http://foo) [http://foo] <http://foo>.
            // The link is the URL, not the brackets around it.
            (r"\((\w+://\S+)\)", "$1", 1),
            (r"\[(\w+://\S+)\]", "$1", 1),
            (r"<(\w+://\S+)>", "$1", 1),
            // A bare URL ending in a balanced parenthesis -- Wikipedia's
            // `/wiki/Rust_(language)` style. 0.57 wrote the boundary after the
            // `)` as a lookahead; the regex crate does not do lookahead, so a
            // consuming group draws the same boundary and the highlight keeps
            // the boundary character out of the link.
            (r"\b(\w+://[^\s()]*\(\S*\))([^_/a-zA-Z0-9-]|$)", "$1", 1),
            // A bare URL, stopping before trailing punctuation: a URL at the
            // end of a sentence should not swallow the full stop.
            (r"\b\w+://\S+[_/a-zA-Z0-9-]", "$0", 0),
            // An email address becomes a mailto link.
            (r"\b\w+@[\w-]+(\.[\w-]+)+\b", "mailto:$0", 0),
        ];
        Rules {
            rules: rules
                .iter()
                .map(|(pattern, format, highlight)| {
                    Rule::new(pattern, format, *highlight).expect("built-in rules must compile")
                })
                .collect(),
            user_defined: false,
        }
    }
}

const SCHEMES: &[&str] = &["https://", "http://", "file://", "ftp://", "mailto:"];

/// Whether a URI's scheme is one the whitelist trusts to reach the desktop.
fn vetted(uri: &str) -> bool {
    SCHEMES.iter().any(|scheme| uri.starts_with(scheme))
}

/// Every link on a row: the ones a program marked, and the ones it printed.
///
/// Marked links win where they overlap. A program that took the trouble to
/// say what a span points at knows better than a pattern does -- the text can
/// be a title while the link goes somewhere else entirely.
pub fn links_in_row(row: usize, line: &StyledScreenLine, rules: &Rules) -> Vec<Link> {
    let mut links = marked_links(row, line);
    let taken: Vec<(usize, usize)> = links.iter().map(|link| (link.start, link.end)).collect();

    for found in printed_links(row, line, rules) {
        let overlaps = taken
            .iter()
            .any(|(start, end)| found.start < *end && found.end > *start);
        if !overlaps {
            links.push(found);
        }
    }
    links.sort_by_key(|link| link.start);
    links
}

/// Links a program marked with OSC 8.
fn marked_links(row: usize, line: &StyledScreenLine) -> Vec<Link> {
    let mut links: Vec<Link> = Vec::new();
    let mut column = 0usize;
    for cell in &line.cells {
        // Spacers after wide characters hold no columns of their own.
        if cell.width == 0 {
            continue;
        }
        let width = cell.width;
        if let Some(uri) = cell.style.hyperlink.as_deref() {
            match links.last_mut() {
                // The same link continuing: one span, not one per cell.
                Some(last) if last.end == column && last.uri == uri => last.end = column + width,
                _ => links.push(Link {
                    row,
                    start: column,
                    end: column + width,
                    uri: uri.to_string(),
                    user_rule: false,
                }),
            }
        }
        column += width;
    }
    links
}

/// URLs a program merely printed, found by the rules.
fn printed_links(row: usize, line: &StyledScreenLine, rules: &Rules) -> Vec<Link> {
    // A byte offset and a column per character, so a match in the text maps
    // back to the grid -- a wide character occupies two columns and one
    // `char`, and the regex speaks in bytes.
    let mut text = String::new();
    let mut starts: Vec<usize> = Vec::new();
    let mut columns: Vec<usize> = Vec::new();
    let mut column = 0usize;
    for cell in &line.cells {
        // Spacers after wide characters hold no columns of their own.
        if cell.width == 0 {
            continue;
        }
        starts.push(text.len());
        columns.push(column);
        text.push(cell.ch);
        column += cell.width;
    }
    starts.push(text.len());
    columns.push(column);

    let mut matches: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for rule in &rules.rules {
        for captures in rule.regex.captures_iter(&text) {
            let Some(shown) = captures.get(rule.highlight) else {
                continue;
            };
            if shown.range().is_empty() {
                continue;
            }
            matches.push((shown.range(), rule.expand(&captures)));
        }
    }
    // Longest first, so where two rules claim the same text the more complete
    // match is the link and the fragment inside it is dropped.
    matches.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.start.cmp(&b.0.start)));

    let mut links: Vec<Link> = Vec::new();
    let mut taken: Vec<std::ops::Range<usize>> = Vec::new();
    for (range, uri) in matches {
        // The built-in rules only underline what the opener would accept; an
        // underline over a link that then refuses to open is a broken promise.
        if !rules.user_defined && !vetted(&uri) {
            continue;
        }
        if taken
            .iter()
            .any(|held| range.start < held.end && range.end > held.start)
        {
            continue;
        }
        let (Ok(start), Ok(end)) = (
            starts.binary_search(&range.start),
            starts.binary_search(&range.end),
        ) else {
            continue;
        };
        taken.push(range);
        links.push(Link {
            row,
            start: columns[start],
            end: columns[end],
            uri,
            user_rule: rules.user_defined,
        });
    }
    links
}

/// Whether a click should open a link.
///
/// Behind Ctrl, because a terminal where a stray click launches a browser is
/// a terminal you cannot select text in.
pub fn opens_on_click(ctrl: bool) -> bool {
    ctrl
}

/// Open the link a click landed on.
///
/// A URI built by the user's own rule opens exactly as the format string
/// wrote it -- the rule is the user saying what to open. Everything else, the
/// OSC 8 kind included, goes through the whitelist in [`open`].
pub fn open_link(link: &Link) -> std::io::Result<()> {
    if link.user_rule {
        launch(&link.uri)
    } else {
        open(&link.uri)
    }
}

/// Hand a URI to the desktop.
///
/// Only schemes we recognise: a cell can claim any string as its link, and
/// handing an arbitrary one to the shell is how a printed line becomes a
/// command someone else chose.
pub fn open(uri: &str) -> std::io::Result<()> {
    if !vetted(uri) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to open a link with an unrecognised scheme: {uri}"),
        ));
    }
    launch(uri)
}

fn launch(uri: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // `start` needs an empty title first, or it treats the URL as one.
        // And no console: this is a GUI binary, so `cmd` would otherwise flash
        // a black window every time a link is clicked.
        crate::git::hidden_command("cmd")
            .args(["/C", "start", "", uri])
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(uri)
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(uri)
            .spawn()
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unterm_engine::{CellStyle, StyledCell};

    fn line(text: &str, link: Option<&str>) -> StyledScreenLine {
        let mut style = CellStyle::default();
        style.hyperlink = link.map(String::from);
        StyledScreenLine {
            row: 0,
            wrapped: false,
            cells: text
                .chars()
                .map(|ch| StyledCell {
                    ch,
                    style: style.clone(),
                    width: 1,
                    combining: None,
                })
                .collect(),
        }
    }

    fn built_in(text: &str) -> Vec<Link> {
        links_in_row(0, &line(text, None), &Rules::built_in())
    }

    fn user_rules(source: &str) -> Rules {
        let config =
            unterm_engine::next_core::config::parse(source).expect("test config should parse");
        Rules::from_config(&config)
    }

    #[test]
    fn a_printed_url_is_a_link() {
        let links = built_in("see https://example.com now");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://example.com");
        assert_eq!(links[0].start, 4);
    }

    #[test]
    fn a_url_at_the_end_of_a_sentence_does_not_swallow_the_full_stop() {
        let links = built_in("go to https://example.com.");
        assert_eq!(links[0].uri, "https://example.com");
    }

    #[test]
    fn a_url_in_brackets_stops_at_the_bracket() {
        for text in [
            "(https://example.com)",
            "[https://example.com]",
            "<https://example.com>",
        ] {
            let links = built_in(text);
            assert_eq!(links[0].uri, "https://example.com", "{text}");
            assert_eq!((links[0].start, links[0].end), (1, 20), "{text}");
        }
    }

    #[test]
    fn a_url_may_end_in_a_balanced_parenthesis() {
        // Wikipedia's `/wiki/Rust_(language)` style: the `)` belongs to the
        // URL, while a bracket merely wrapping one does not.
        let links = built_in("read https://en.wikipedia.org/wiki/Rust_(language) today");
        assert_eq!(
            links[0].uri,
            "https://en.wikipedia.org/wiki/Rust_(language)"
        );
    }

    #[test]
    fn an_email_address_becomes_a_mailto_link() {
        let links = built_in("written by foo@example.com yesterday");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "mailto:foo@example.com");
        // And mailto is a scheme the opener accepts, so the underline is not
        // a promise the click then breaks.
        assert!(!links[0].user_rule);
    }

    #[test]
    fn a_bare_scheme_is_not_a_link() {
        assert!(built_in("https://").is_empty());
    }

    #[test]
    fn several_urls_on_a_row_are_all_found() {
        let links = built_in("http://a.com and http://b.com");
        assert_eq!(links.len(), 2);
        assert_eq!(links[1].uri, "http://b.com");
    }

    #[test]
    fn the_built_in_rules_only_find_what_the_opener_would_accept() {
        // `ssh://` matches the URL pattern, but the opener would refuse it,
        // and an underline over a link that will not open is a broken promise.
        assert!(built_in("connect to ssh://host/path now").is_empty());
    }

    #[test]
    fn a_user_rule_builds_its_uri_from_the_captures() {
        let rules = user_rules(
            r#"hyperlink_rules = [["\b[Ii]ssue #?(\d+)", "https://example.com/issues/$1"]]"#,
        );
        let links = links_in_row(0, &line("fixes issue #123 for good", None), &rules);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://example.com/issues/123");
        assert_eq!((links[0].start, links[0].end), (6, 16));
        assert!(links[0].user_rule);
    }

    #[test]
    fn user_rules_replace_the_built_in_set_entirely() {
        // Exactly as 0.57 behaved: writing any rules means writing all of
        // them, so a URL is no longer a link unless a rule says so.
        let rules = user_rules(r#"hyperlink_rules = [["\bPR-(\d+)", "https://example.com/$1"]]"#);
        let links = links_in_row(0, &line("see https://example.com and PR-7", None), &rules);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://example.com/7");
    }

    #[test]
    fn an_empty_rule_list_turns_printed_links_off() {
        let rules = user_rules("hyperlink_rules = []");
        assert!(links_in_row(0, &line("https://example.com", None), &rules).is_empty());
    }

    #[test]
    fn a_user_rule_may_open_a_scheme_the_whitelist_does_not() {
        // The rule's format string is the user saying what to open.
        let rules = user_rules(r#"hyperlink_rules = [["\bssh://\S+", "$0"]]"#);
        let links = links_in_row(0, &line("connect to ssh://host/path", None), &rules);
        assert_eq!(links[0].uri, "ssh://host/path");
        assert!(links[0].user_rule, "open_link opens this one as written");
    }

    #[test]
    fn a_third_element_names_the_capture_to_highlight() {
        let rules = user_rules(r#"hyperlink_rules = [["fetch <(\S+)>", "https://$1", 1]]"#);
        let links = links_in_row(0, &line("fetch <example.com> now", None), &rules);
        assert_eq!(links[0].uri, "https://example.com");
        // Only the capture is underlined, not the words around it.
        assert_eq!((links[0].start, links[0].end), (7, 18));
    }

    #[test]
    fn where_two_rules_claim_the_same_text_the_longer_match_is_the_link() {
        // The bracket rule and the bare-URL rule both see this URL; one link
        // comes out, not a link and a fragment inside it.
        let links = built_in("(https://example.com/page)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://example.com/page");
    }

    #[test]
    fn a_marked_link_spans_its_cells_as_one() {
        let links = links_in_row(
            0,
            &line("click", Some("https://example.com")),
            &Rules::built_in(),
        );
        assert_eq!(links.len(), 1);
        assert_eq!((links[0].start, links[0].end), (0, 5));
        assert_eq!(links[0].uri, "https://example.com");
    }

    #[test]
    fn a_marked_link_wins_over_the_text_that_looks_like_one() {
        // The text can be a title while the link goes somewhere else, and the
        // program that said so knows better than a pattern does.
        let line = line("https://decoy.example", Some("https://real.example"));
        let links = links_in_row(0, &line, &Rules::built_in());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://real.example");
    }

    #[test]
    fn a_click_only_opens_with_the_modifier() {
        assert!(
            !opens_on_click(false),
            "a stray click must not open a browser"
        );
        assert!(opens_on_click(true));
    }

    #[test]
    fn an_unrecognised_scheme_is_refused_rather_than_handed_to_the_shell() {
        // A cell can claim any string as its link.
        for uri in [
            "javascript:alert(1)",
            "vbscript:x",
            "data:text/html,<script>",
            "\\\\evil\\share",
            "C:\\Windows\\System32\\calc.exe",
        ] {
            assert!(
                open(uri).is_err(),
                "{uri} should not reach the desktop opener"
            );
        }
    }

    #[test]
    fn a_marked_link_stays_behind_the_whitelist_even_with_user_rules() {
        // The rules changing does not change what an OSC 8 cell may claim.
        let rules = user_rules(r#"hyperlink_rules = [["\bssh://\S+", "$0"]]"#);
        let links = links_in_row(0, &line("click", Some("javascript:alert(1)")), &rules);
        assert!(!links[0].user_rule);
        assert!(open_link(&links[0]).is_err());
    }

    #[test]
    fn covering_a_column_is_half_open_at_the_end() {
        let link = Link {
            row: 2,
            start: 4,
            end: 8,
            uri: "https://example.com".to_string(),
            user_rule: false,
        };
        assert!(link.covers(2, 4));
        assert!(link.covers(2, 7));
        assert!(!link.covers(2, 8), "the end column is past the link");
        assert!(!link.covers(3, 5), "another row is another link");
    }

    #[test]
    fn a_wide_character_before_a_url_shifts_its_columns() {
        let mut line = line("中 https://example.com", None);
        line.cells[0].width = 2;
        let links = links_in_row(0, &line, &Rules::built_in());
        assert_eq!(
            links[0].start, 3,
            "two columns for the character, one for the space"
        );
    }
}
