//! The bar along the top: the wordmark, what the pane is doing, and the
//! actions.
//!
//! One row. The tabs are not here -- they live in the strip down the left,
//! where a tab's identity (a project and a command) fits along a row instead of
//! across one. What is left is what a window needs at the top: its name, the
//! facts about the pane in front, the handful of things reached constantly, and
//! the buttons that close it.
//!
//! Laid out in pixels rather than in terminal cells, and measured through the
//! caller's own font, because the chrome is drawn in a proportional face: a
//! label placed on a grid comes out spread across it, and a bar laid out on one
//! grid and hit-tested on another has buttons that are not where they look.
//!
//! What drops as the window narrows is ported from the previous front end,
//! thresholds included. The order matters more than the numbers: labels go
//! first, then the two navigation icons, then split. The command palette, the
//! project toggle, the settings gear and the menu never go, and neither do the
//! window buttons -- a window with no title bar has no other way to be closed.

use crate::keys::Action;

/// How tall the bar is: one chrome row with a little air around it.
pub fn height(_row_height: f32, pt: f32, quiet: bool) -> f32 {
    // v0.57.4 reserved 1.6 title-font cells for the integrated chrome. The
    // first next-core version added a fully padded sidebar row *and* another
    // 8pt around it, producing the visibly oversized ~77px strip at 150% DPI.
    let cell = crate::ui_tokens::UI_FONT_SIZE as f32 * pt;
    // 2.42 cells was measured off v0.57.4, whose bar held a full row of
    // action chips and needed the room. The quiet bar matches the platform's
    // own title bar instead of inventing a height: two cells lands on
    // Windows' 32 logical pixels, but on macOS a point is only the scale, so
    // the same arithmetic came out 24 -- four short of AppKit's 28, which is
    // the height it centers the native traffic lights for. A bar shorter
    // than that wears its lights visibly low.
    let base = if quiet {
        if cfg!(target_os = "macos") {
            28.0 * pt
        } else {
            cell * 2.0
        }
    } else {
        cell * 2.4
    };
    // The v0.57.4 quick-action cells were the taller constraint: one title
    // cell plus 0.18-cell margins and 0.22-cell padding on both sides, with a
    // physical-pixel border. At 150% DPI this is ~49px.
    let buttons = cell * (1.0 + 2.0 * (0.18 + 0.22)) + 2.0;
    base.max(buttons).ceil()
}

/// What the bar shows at a given width, in logical pixels.
///
/// Ported thresholds. Labels are the first thing to go because an icon still
/// says what it does; the navigation pair goes next because the palette can
/// reach both; split goes last of the optional ones because it is the one
/// people press without looking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Density {
    pub show_split: bool,
    pub show_navigation: bool,
    pub show_labels: bool,
}

pub fn density(logical_width: f32) -> Density {
    Density {
        show_split: logical_width >= 600.0,
        show_navigation: logical_width >= 760.0,
        show_labels: logical_width >= 1200.0,
    }
}

/// The name in the corner. Not clickable; it is there so a window with no
/// title bar says what it is.
pub const WORDMARK: &str = "Unterm";

/// Whether the platform lays its own window buttons over the chrome.
///
/// On macOS the window keeps its native frame with a transparent title bar,
/// so the traffic lights are real controls at the top-left -- the bar must
/// not draw its own buttons, and must start its content clear of them.
pub fn native_traffic_lights() -> bool {
    cfg!(target_os = "macos")
}

/// The actions on the right, in the order they are drawn.
///
/// Code points from the bundled symbols face, as the previous front end used
/// them: three bars for the palette, a left sidebar for the project strip, a
/// horizontal split, a folder, a magnifier, a gear.
pub const ACTIONS: &[(Action, char, &str)] = &[
    (Action::CommandPalette, '\u{eb6a}', "menu.command_palette"),
    (Action::TreeSidebar, '\u{ebf3}', "menu.tree_sidebar"),
    (Action::SplitRight, '\u{eb56}', ""),
    (Action::DirJump, '\u{ea83}', ""),
    (Action::Search, '\u{ea6d}', ""),
    (Action::Settings, '\u{eb51}', ""),
];

/// The quick menu at the far right of the actions — the codicon chevron the
/// previous front end used, not the text triangle.
pub const MENU: char = '\u{eab4}';

/// What a piece of the bar is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Wordmark,
    /// What this window is showing, across the middle. Text, not a
    /// control: pressing it drags the window, as any title bar does.
    Title,
    /// The line of facts about the pane in front. Text, not a control.
    Stats,
    /// The Agent Cockpit tally: agents seen, and how many wait for a person.
    /// Pressing it opens the inbox.
    Cockpit,
    Action(Action),
    Menu,
    Minimise,
    Maximise,
    Close,
}

/// A piece of the bar, placed.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub item: Item,
    pub left: f32,
    pub width: f32,
    /// What to draw: an icon, a label, or both.
    pub icon: Option<char>,
    pub label: String,
    /// What to say when the pointer rests on it, for the icon-only ones.
    pub tooltip: Option<String>,
}

impl Placed {
    pub fn contains(&self, x: f32) -> bool {
        x >= self.left && x < self.left + self.width
    }
}

/// Lay the bar out.
///
/// `measure` gives the width of a piece of text in the face it will be drawn
/// in; `pt` is one point in pixels on this display. The window buttons come
/// first in the arithmetic and last in the list, because they are the one thing
/// that must never move or be dropped.
pub fn layout(
    width: f32,
    logical_width: f32,
    pt: f32,
    stats: &str,
    cockpit: &str,
    title: &str,
    project_open: bool,
    quiet: bool,
    measure: &mut dyn FnMut(&str) -> f32,
) -> Vec<Placed> {
    let mut density = density(logical_width);
    // The quiet bar is the inbox design's default: the brand, the
    // cockpit tally, the menu chevron and the window buttons — every
    // action keeps its chord, its palette row and its menu entry, so
    // nothing is lost, only unhung from the chrome. `top_bar = "full"`
    // in the config restores the action row and the facts line.
    if quiet {
        density.show_split = false;
        density.show_navigation = false;
        density.show_labels = false;
    }
    let mut placed = Vec::new();
    if width <= 0.0 {
        return placed;
    }

    // The window buttons, from the right edge inwards, in the order the
    // platform puts them.
    let button = button_width(pt);
    let mut right = width;
    let mut buttons = Vec::new();
    if !native_traffic_lights() {
        for item in [Item::Close, Item::Maximise, Item::Minimise] {
            right -= button;
            buttons.push(Placed {
                item,
                left: right,
                width: button,
                icon: None,
                label: String::new(),
                tooltip: None,
            });
        }
    } else {
        // The platform draws its lights on the left with a deliberate
        // margin; with no buttons of ours on the right, the chevron would
        // otherwise start at the raw edge and sit inside the window's
        // rounded corner. Mirror the lights' breathing room.
        right -= (10.0 * pt).round();
    }

    // Then the actions, right to left, so the menu sits closest to the buttons.
    // 0.57.4's action rhythm: 0.24 cells between chips, 0.44 inside them.
    let gap = (3.0 * pt).round();
    let pad = (6.0 * pt).round();
    let mut actions = Vec::new();

    let mut take =
        |item: Item, icon: char, label: &str, tooltip: Option<String>, right: &mut f32| {
            let text = if label.is_empty() {
                icon.to_string()
            } else {
                format!("{icon}  {label}")
            };
            let wide = (measure(&text) + pad * 2.0).round();
            if *right - wide < 0.0 {
                return;
            }
            *right -= wide + gap;
            actions.push(Placed {
                item,
                left: *right + gap,
                width: wide,
                icon: Some(icon),
                label: label.to_string(),
                tooltip,
            });
        };

    take(
        Item::Menu,
        MENU,
        "",
        // The chevron is the menu affordance itself.  0.57.4 did not leave a
        // second "More actions" label floating over the dropdown after it
        // was pressed.
        None,
        &mut right,
    );

    for (action, icon, label_key) in ACTIONS.iter().rev() {
        let wanted = if quiet {
            false
        } else {
            match action {
                Action::SplitRight => density.show_split,
                Action::DirJump | Action::Search => density.show_navigation,
                _ => true,
            }
        };
        if !wanted {
            continue;
        }
        // Only the two that carry one, and only when there is room for words.
        let label = if density.show_labels && !label_key.is_empty() {
            unterm_services::i18n::t(label_key)
        } else {
            String::new()
        };
        // An icon with no words needs a name on hover, or nobody learns what
        // it does without pressing it.
        let tooltip = if label.is_empty() {
            Some(match label_key {
                key if !key.is_empty() => unterm_services::i18n::t(key),
                _ => action.label().to_string(),
            })
        } else {
            None
        };
        take(Item::Action(*action), *icon, &label, tooltip, &mut right);
    }

    // The Cockpit tally sits just left of the actions, when there is one:
    // an agent waiting for a person has to be visible from any tab.
    if !cockpit.trim().is_empty() {
        take(Item::Cockpit, '⚡', cockpit, None, &mut right);
    }
    let _ = project_open;

    // The wordmark on the left.
    let mut left = (crate::ui_tokens::CHROME_PANEL_INSET * pt).round();
    if native_traffic_lights() {
        // Clear the traffic-light cluster: a 7px margin, three 14px buttons
        // on 20px centres, and air after -- ~72 logical px, converted with
        // the window's own scale because `pt` is a typographic point.
        let scale = if logical_width > 0.0 {
            width / logical_width
        } else {
            1.0
        };
        left += (72.0 * scale).round();
    }
    // On macOS the corner already says whose window this is -- the traffic
    // lights, the Dock icon, the app menu. 0.57.4 put no brand in the bar
    // there, and neither does any native Mac app; the mark stays where a
    // Windows or Linux window genuinely has no other identity.
    if !native_traffic_lights() {
        // v0.57.4 sized the Command Loop mark to 0.95 title-font cells and
        // separated it from the word by another 0.42 cell. Measuring the
        // actual UI face keeps that relationship intact for Segoe and Inter.
        let brand_cell = measure("M");
        // The old box model gave the brand another 0.3 cell before the mark.
        // Without it the icon hugs the window edge while the wordmark pulls
        // right, making the pair look off-centre even when their vertical
        // metrics agree.
        left += brand_cell * 0.3;
        match crate::window_buttons::style() {
            // Windows: the app's mark and then the window's title, as every
            // Fluent title bar has them -- the icon, not the product's name
            // spelled out beside it. The word read as a web page's header.
            crate::window_buttons::Style::Fluent => {
                let mark = brand_cell * (0.95 + 0.9);
                if left + mark < right {
                    placed.push(Placed {
                        item: Item::Wordmark,
                        left,
                        width: mark,
                        icon: None,
                        label: String::new(),
                        tooltip: None,
                    });
                    left += mark;
                }
            }
            // GNOME's header bars carry no brand at all: the title, centred,
            // and the buttons. The window list already says whose window it is.
            crate::window_buttons::Style::Adwaita => {}
        }
    }

    // The middle: what this window is showing. A bar with a brand at
    // one end, controls at the other and nothing in between reads as
    // an empty band -- and which window this is happens to be the one
    // thing anybody ever wanted from a title bar. The full bar gives
    // the room to its facts line instead.
    let title = title.trim();
    if quiet && !title.is_empty() {
        let air = (12.0 * pt).round();
        let room_left = left + air;
        let room_right = right - air;
        let wide = measure(title);
        // Fluent puts the title straight after the icon; macOS and GNOME
        // centre it.
        let left_aligned = !native_traffic_lights()
            && crate::window_buttons::style() == crate::window_buttons::Style::Fluent;
        if wide <= (room_right - room_left).max(0.0) {
            let at = if left_aligned {
                left
            } else {
                ((width - wide) / 2.0).clamp(room_left, room_right - wide)
            };
            placed.push(Placed {
                item: Item::Title,
                left: at,
                width: wide,
                icon: None,
                label: title.to_string(),
                tooltip: None,
            });
        }
    }

    // And the facts, in whatever is between. Right-aligned against the
    // actions, because that is where the eye goes back to after reading them.
    // The quiet bar leaves them to the shell's own prompt.
    let stats = if quiet { "" } else { stats.trim() };
    if !stats.is_empty() {
        let wide = measure(stats);
        let start = right - wide - (8.0 * pt).round();
        if start > left + (12.0 * pt).round() {
            placed.push(Placed {
                item: Item::Stats,
                left: start,
                width: wide,
                icon: None,
                label: stats.to_string(),
                tooltip: None,
            });
        }
    }

    actions.reverse();
    placed.extend(actions);
    placed.extend(buttons);
    placed
}

/// The window button this piece is, if it is one.
pub fn window_button(item: Item, maximized: bool) -> Option<crate::window_buttons::Button> {
    match item {
        Item::Minimise => Some(crate::window_buttons::Button::Minimise),
        Item::Maximise if maximized => Some(crate::window_buttons::Button::Restore),
        Item::Maximise => Some(crate::window_buttons::Button::Maximise),
        Item::Close => Some(crate::window_buttons::Button::Close),
        _ => None,
    }
}

/// Which piece a press at `x` landed on.
pub fn hit(placed: &[Placed], x: f32) -> Option<Item> {
    if let Some(piece) = placed.iter().find(|piece| piece.contains(x)) {
        return Some(piece.item);
    }
    // On the platform whose window buttons are the system's, the chevron is
    // the rightmost thing we draw, inset from the corner so it clears the
    // window's rounding. The strip between it and the edge stays part of its
    // target: a corner is the easiest place on a screen to throw the pointer,
    // and years of the chevron living flush against it taught hands to do
    // exactly that.
    if native_traffic_lights() {
        if let Some(menu) = placed.iter().filter(|piece| piece.item == Item::Menu).last() {
            let rightmost = placed
                .iter()
                .all(|piece| piece.left + piece.width <= menu.left + menu.width + 0.5);
            if rightmost && x >= menu.left + menu.width {
                return Some(Item::Menu);
            }
        }
    }
    None
}

/// Whether a press here should drag the window.
///
/// Everywhere the bar has nothing in it, plus the two pieces that are text
/// rather than controls. A title bar you cannot drag the window by is the first
/// thing anyone notices about a window that has none.
pub fn is_drag_handle(placed: &[Placed], x: f32) -> bool {
    match hit(placed, x) {
        None | Some(Item::Wordmark) | Some(Item::Title) => true,
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximize_caption_reflects_the_window_state() {
        assert_eq!(
            window_button(Item::Maximise, false),
            Some(crate::window_buttons::Button::Maximise)
        );
        assert_eq!(
            window_button(Item::Maximise, true),
            Some(crate::window_buttons::Button::Restore)
        );
    }

    /// A stand-in for a proportional face: every character the same width, but
    /// a width that is not a terminal cell, so nothing can accidentally depend
    /// on the grid.
    fn measurer() -> impl FnMut(&str) -> f32 {
        |text: &str| text.chars().count() as f32 * 7.0
    }

    fn bar(width: f32) -> Vec<Placed> {
        let mut measure = measurer();
        layout(width, width, 1.33, "", "", "", true, false, &mut measure)
    }

    fn has(bar: &[Placed], item: Item) -> bool {
        bar.iter().any(|piece| piece.item == item)
    }

    /// On macOS the corner belongs to the native traffic lights: the bar
    /// draws no buttons of its own, and its content starts clear of them.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_traffic_lights_keep_their_corner() {
        let bar = bar(1400.0);
        for item in [Item::Close, Item::Maximise, Item::Minimise] {
            assert!(!has(&bar, item), "{item:?} should be the system's");
        }
        // And no brand beside them: the corner already says whose window
        // this is, the way 0.57.4 and every native Mac app leave it.
        assert!(
            !has(&bar, Item::Wordmark),
            "the wordmark crowds the traffic lights"
        );
    }

    /// The buttons that close the window are never dropped and never move: a
    /// window with no title bar has no other way to be closed.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_window_buttons_survive_every_width() {
        for width in [80.0, 200.0, 600.0, 1400.0, 3000.0] {
            let bar = bar(width);
            for item in [Item::Close, Item::Maximise, Item::Minimise] {
                assert!(has(&bar, item), "{item:?} was dropped at {width}");
            }
            let close = bar.iter().find(|piece| piece.item == Item::Close).unwrap();
            assert!(
                close.left + close.width <= width + 0.5,
                "the close button is off the edge at {width}"
            );
        }
    }

    /// Nothing ever overlaps. A press landing on a button drawn underneath
    /// another closes the window by accident.
    #[test]
    fn no_two_pieces_overlap() {
        for width in (60..2400).step_by(37) {
            let mut measure = measurer();
            let bar = layout(
                width as f32,
                width as f32,
                1.33,
                "\u{26A1} claude    main *3    8.0% cpu  1.4G",
                "",
                "",
                true,
                false,
                &mut measure,
            );
            for (index, piece) in bar.iter().enumerate() {
                for other in &bar[index + 1..] {
                    let apart = piece.left + piece.width <= other.left + 0.01
                        || other.left + other.width <= piece.left + 0.01;
                    assert!(
                        apart,
                        "at {width}: {:?} and {:?} overlap",
                        piece.item, other.item
                    );
                }
            }
        }
    }

    /// The palette, the project toggle, the gear and the menu are always
    /// reachable: they are how everything else is reached.
    #[test]
    fn the_essential_actions_are_never_dropped() {
        for width in [400.0, 800.0, 1600.0] {
            let bar = bar(width);
            for action in [
                Action::CommandPalette,
                Action::TreeSidebar,
                Action::Settings,
            ] {
                assert!(
                    has(&bar, Item::Action(action)),
                    "{action:?} was dropped at {width}"
                );
            }
            assert!(has(&bar, Item::Menu), "the menu was dropped at {width}");
        }
    }

    /// What drops, drops in order: labels, then the navigation pair, then
    /// split. Ported thresholds.
    #[test]
    fn things_drop_in_the_order_they_were_given() {
        assert!(!density(500.0).show_split);
        assert!(density(600.0).show_split);
        assert!(!density(700.0).show_navigation);
        assert!(density(760.0).show_navigation);
        assert!(!density(1000.0).show_labels);
        assert!(density(1200.0).show_labels);
    }

    /// Only the two that have words carry them, and only when there is room.
    #[test]
    fn labels_appear_only_on_the_two_that_have_them() {
        let mut measure = measurer();
        let wide = layout(1600.0, 1600.0, 1.33, "", "", "", true, false, &mut measure);
        let labelled: Vec<Action> = wide
            .iter()
            .filter(|piece| !piece.label.is_empty())
            .filter_map(|piece| match piece.item {
                Item::Action(action) => Some(action),
                _ => None,
            })
            .collect();
        assert_eq!(labelled, vec![Action::CommandPalette, Action::TreeSidebar]);

        let mut measure = measurer();
        let narrow = layout(900.0, 900.0, 1.33, "", "", "", true, false, &mut measure);
        assert!(
            narrow
                .iter()
                .filter(|piece| matches!(piece.item, Item::Action(_)))
                .all(|piece| piece.label.is_empty()),
            "a label survived a narrow bar"
        );
    }

    /// An icon with no words says what it is on hover. Without that, nobody
    /// learns what it does except by pressing it.
    #[test]
    fn an_icon_with_no_words_has_something_to_say_on_hover() {
        let mut measure = measurer();
        let bar = layout(900.0, 900.0, 1.33, "", "", "", true, false, &mut measure);
        for piece in &bar {
            if matches!(piece.item, Item::Action(_)) && piece.label.is_empty() {
                let tooltip = piece.tooltip.as_deref().unwrap_or("");
                assert!(!tooltip.trim().is_empty(), "{:?} has no name", piece.item);
            }
        }
    }

    /// The actions read left to right in the order they are declared, whatever
    /// order they were placed in: the menu is at the far right.
    #[test]
    fn the_actions_read_in_the_declared_order() {
        let mut measure = measurer();
        let bar = layout(1600.0, 1600.0, 1.33, "", "", "", true, false, &mut measure);
        let order: Vec<Item> = bar
            .iter()
            .filter(|piece| matches!(piece.item, Item::Action(_) | Item::Menu))
            .map(|piece| piece.item)
            .collect();
        let mut wanted: Vec<Item> = ACTIONS
            .iter()
            .map(|(action, _, _)| Item::Action(*action))
            .collect();
        wanted.push(Item::Menu);
        assert_eq!(order, wanted);
    }

    /// No tabs. They live in the strip, and a tab in two places is a tab whose
    /// selection can disagree with itself.
    #[test]
    fn the_bar_carries_no_tabs() {
        let bar = bar(1600.0);
        assert!(bar.iter().all(|piece| !matches!(
            piece.item,
            Item::Wordmark if piece.label.chars().all(char::is_numeric)
        )));
        // The only things here are the wordmark, the facts, actions, the menu
        // and the window buttons.
        for piece in &bar {
            assert!(
                matches!(
                    piece.item,
                    Item::Wordmark
                        | Item::Stats
                        | Item::Action(_)
                        | Item::Menu
                        | Item::Minimise
                        | Item::Maximise
                        | Item::Close
                ),
                "{:?} does not belong in the top bar",
                piece.item
            );
        }
    }

    /// The facts sit between the wordmark and the actions, and are dropped
    /// rather than allowed to collide with either.
    #[test]
    fn the_facts_sit_between_the_wordmark_and_the_actions() {
        let mut measure = measurer();
        let bar = layout(
            1600.0,
            1600.0,
            1.33,
            "8.0% cpu  1.4G  4m",
            "",
            "",
            true,
            false,
            &mut measure,
        );
        let stats = bar
            .iter()
            .find(|piece| piece.item == Item::Stats)
            .expect("a wide bar has room for the facts");
        // On macOS there is no wordmark to sit after; the facts still keep
        // clear of whatever the bar starts with.
        let content_start = bar
            .iter()
            .find(|piece| piece.item == Item::Wordmark)
            .map(|wordmark| wordmark.left + wordmark.width)
            .unwrap_or(0.0);
        let first_action = bar
            .iter()
            .filter(|piece| matches!(piece.item, Item::Action(_)))
            .map(|piece| piece.left)
            .fold(f32::MAX, f32::min);
        assert!(stats.left > content_start);
        assert!(stats.left + stats.width <= first_action + 0.5);
    }

    /// A narrow bar gives the facts up rather than pushing anything else off.
    #[test]
    fn a_narrow_bar_gives_up_the_facts() {
        let mut measure = measurer();
        let bar = layout(
            360.0,
            360.0,
            1.33,
            "\u{26A1} claude    main *3    8.0% cpu  1.4G  4m",
            "",
            "",
            true,
            false,
            &mut measure,
        );
        assert!(!has(&bar, Item::Stats));
        // Off macOS the close button holds its corner; on it, the corner is
        // the traffic lights' and there is nothing of ours to keep.
        assert_eq!(has(&bar, Item::Close), !native_traffic_lights());
    }

    /// The wordmark and empty bar drag the window. Pane facts are a control:
    /// pressing the displayed shell opens the shell selector.
    #[test]
    fn the_text_drags_and_the_controls_do_not() {
        let mut measure = measurer();
        let bar = layout(1600.0, 1600.0, 1.33, "8.0% cpu", "", "", true, false, &mut measure);
        for piece in &bar {
            let middle = piece.left + piece.width / 2.0;
            let expected = matches!(piece.item, Item::Wordmark);
            assert_eq!(
                is_drag_handle(&bar, middle),
                expected,
                "{:?} drags the window: {expected}",
                piece.item
            );
        }
        // And the empty space between them does too.
        assert!(is_drag_handle(&bar, 1.0));
    }

    /// A bar with no width draws nothing rather than a row of pieces at
    /// negative positions.
    #[test]
    fn a_bar_with_no_width_is_empty() {
        assert!(bar(0.0).is_empty());
        assert!(bar(-10.0).is_empty());
    }
}

/// Which tab a number key means, counting from one as the keys are labelled.
///
/// Nine is the last tab however many there are, which is what every browser
/// does -- someone with four tabs pressing Ctrl+9 means the last one, not
/// nothing. Other numbers past the end are nothing rather than the last one:
/// Ctrl+3 with two tabs open is a miss, and jumping somewhere unasked-for is
/// worse than staying put.
pub fn tab_for_number(number: u8, count: usize) -> Option<usize> {
    if count == 0 || number == 0 {
        return None;
    }
    if number >= 9 {
        return Some(count - 1);
    }
    let index = number as usize - 1;
    (index < count).then_some(index)
}

#[cfg(test)]
mod number_key_tests {
    use super::*;

    #[test]
    fn a_number_picks_the_tab_with_that_position() {
        assert_eq!(tab_for_number(1, 4), Some(0));
        assert_eq!(tab_for_number(3, 4), Some(2));
    }

    /// Nine is the last one, however many there are.
    #[test]
    fn nine_is_the_last_tab_not_the_ninth() {
        assert_eq!(tab_for_number(9, 4), Some(3));
        assert_eq!(tab_for_number(9, 1), Some(0));
        assert_eq!(tab_for_number(9, 12), Some(11));
    }

    /// And a number past the end is a miss, not a jump to the end -- those
    /// two rules only look inconsistent until you press Ctrl+3 by accident.
    #[test]
    fn a_number_past_the_last_tab_does_nothing() {
        assert_eq!(tab_for_number(3, 2), None);
        assert_eq!(tab_for_number(8, 2), None);
    }

    #[test]
    fn there_is_no_tab_when_there_are_no_tabs() {
        assert_eq!(tab_for_number(1, 0), None);
        assert_eq!(tab_for_number(9, 0), None);
    }
}

/// Which edge of the window the pointer is on, if any.
///
/// A window with no system frame has no system resize handles either, so it
/// has to grow its own. Without them the window is stuck at whatever size it
/// opened at, which is the price of a borderless window nobody mentions until
/// they try to make one wider.
///
/// The band is deliberately wider than a pixel: a one-pixel target is one
/// nobody hits.
pub fn resize_edge(
    pointer: (f32, f32),
    size: (f32, f32),
    pt: f32,
) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection;

    // Measured in points, not device pixels: six device pixels is four
    // logical ones on a 150% display, a target missed often enough that
    // the window reads as one that cannot be resized at all. Corners
    // reach further along both of their edges, the way every window
    // manager grows them, because a corner is what anyone resizing two
    // dimensions at once aims for.
    let band = (6.0 * pt).max(6.0);
    let corner = band * 2.5;
    let left = pointer.0 <= band;
    let right = pointer.0 >= size.0 - band;
    let top = pointer.1 <= band;
    let bottom = pointer.1 >= size.1 - band;
    let near_left = pointer.0 <= corner;
    let near_right = pointer.0 >= size.0 - corner;
    let near_top = pointer.1 <= corner;
    let near_bottom = pointer.1 >= size.1 - corner;

    Some(if (top && near_left) || (left && near_top) {
        ResizeDirection::NorthWest
    } else if (top && near_right) || (right && near_top) {
        ResizeDirection::NorthEast
    } else if (bottom && near_left) || (left && near_bottom) {
        ResizeDirection::SouthWest
    } else if (bottom && near_right) || (right && near_bottom) {
        ResizeDirection::SouthEast
    } else if top {
        ResizeDirection::North
    } else if bottom {
        ResizeDirection::South
    } else if left {
        ResizeDirection::West
    } else if right {
        ResizeDirection::East
    } else {
        return None;
    })
}

/// The arrow that says which way an edge will move.
///
/// Without one the band is invisible: an edge that does not change the
/// pointer is an edge nobody knows is there.
pub fn resize_cursor(direction: winit::window::ResizeDirection) -> winit::window::CursorIcon {
    use winit::window::{CursorIcon, ResizeDirection};
    match direction {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
    }
}

/// How wide one window button is.
pub fn button_width(pt: f32) -> f32 {
    // `pt` is pixels per typographic point, 4/3 at 96dpi: the slot widths
    // are in logical pixels, so three quarters of them are points -- 46px,
    // the Windows caption width, is 34.5pt.
    let logical = crate::window_buttons::slot_width(crate::window_buttons::style());
    (logical * 0.75 * pt).round()
}

/// How far in from the right edge the window buttons reach.
///
/// The resize band has to keep out of this: a press meant to close the
/// window must never start a drag instead.
pub fn window_button_band(pt: f32) -> f32 {
    if native_traffic_lights() {
        0.0
    } else {
        button_width(pt) * 3.0
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;
    use winit::window::ResizeDirection;

    const SIZE: (f32, f32) = (800.0, 600.0);

    #[test]
    fn the_middle_of_the_window_is_not_an_edge() {
        assert_eq!(resize_edge((400.0, 300.0), SIZE, 1.0), None);
    }

    #[test]
    fn each_side_reports_its_own_direction() {
        assert_eq!(resize_edge((0.0, 300.0), SIZE, 1.0), Some(ResizeDirection::West));
        assert_eq!(
            resize_edge((799.0, 300.0), SIZE, 1.0),
            Some(ResizeDirection::East)
        );
        assert_eq!(
            resize_edge((400.0, 0.0), SIZE, 1.0),
            Some(ResizeDirection::North)
        );
        assert_eq!(
            resize_edge((400.0, 599.0), SIZE, 1.0),
            Some(ResizeDirection::South)
        );
    }

    /// The corners resize both ways at once, which is the whole reason to
    /// grab one.
    #[test]
    fn each_corner_resizes_in_two_directions() {
        assert_eq!(
            resize_edge((1.0, 1.0), SIZE, 1.0),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            resize_edge((799.0, 1.0), SIZE, 1.0),
            Some(ResizeDirection::NorthEast)
        );
        assert_eq!(
            resize_edge((1.0, 599.0), SIZE, 1.0),
            Some(ResizeDirection::SouthWest)
        );
        assert_eq!(
            resize_edge((799.0, 599.0), SIZE, 1.0),
            Some(ResizeDirection::SouthEast)
        );
    }

    /// Wide enough to hit. A one-pixel target is one nobody hits, and the
    /// window is then stuck at the size it opened at.
    #[test]
    fn the_edge_is_wide_enough_to_aim_at() {
        for offset in 0..=5 {
            let at = offset as f32;
            assert!(
                resize_edge((at, 300.0), SIZE, 1.0).is_some(),
                "{at} from the left"
            );
            assert!(
                resize_edge((SIZE.0 - at, 300.0), SIZE, 1.0).is_some(),
                "{at} from the right"
            );
        }
    }

    /// But not so wide it swallows the buttons: they sit within a couple of
    /// cells of the top-right corner, and an edge that reached them would
    /// resize the window instead of closing it.
    #[test]
    fn the_edge_does_not_reach_the_window_buttons() {
        assert_eq!(resize_edge((SIZE.0 - 20.0, 20.0), SIZE, 1.0), None);
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    /// Where the terminal starts, below the bar. The window derives this
    /// itself; the test keeps the derivation honest.
    fn terminal_top(row_height: f32, pt: f32, quiet: bool) -> f32 {
        height(row_height, pt, quiet) + (crate::ui_tokens::CHROME_PANEL_INSET * pt).round()
    }

    /// The terminal starts below the bar, with a gap. The `row` argument is a
    /// padded sidebar row, not the title font's bare cell, so the old 1.6-cell
    /// title bar is intentionally allowed to be shorter than it.
    #[test]
    fn the_terminal_starts_below_the_bar_with_a_gap() {
        for (row, pt) in [(24.0, 1.0), (31.0, 1.33), (46.0, 2.0)] {
            let bar = height(row, pt, true);
            let top = terminal_top(row, pt, true);
            assert!(bar >= 20.0 * pt, "the bar is too short at {pt}x: {bar}");
            assert!(top > bar, "the terminal starts inside the bar at {pt}x");
        }
    }

    /// Both measurements grow with the display, because the tokens are in
    /// points: fixed in device pixels they would be a third small on this
    /// machine.
    #[test]
    fn the_bar_grows_with_the_display() {
        let one = height(24.0, 1.0, true);
        let bigger = height(36.0, 1.5, true);
        assert!(bigger > one, "{one} then {bigger}");
    }

    /// A bar is never taller than a comfortable share of a small window: a
    /// chrome that takes half the height of a short window is a window with no
    /// terminal in it.
    #[test]
    fn the_bar_stays_a_small_share_of_a_short_window() {
        let pt = 1.33;
        let bar = height(31.0, pt, true);
        for window in [400.0, 600.0, 900.0] {
            assert!(
                bar < window * 0.2,
                "a {bar}px bar in a {window}px window is too much"
            );
        }
    }
}
