//! What the keys do, in one place.
//!
//! Split out of the event handler for two reasons. An agent asking the MCP
//! surface `meta.keybindings` should get the same answer the window acts on,
//! not a second list that drifts from it. And a chain of `matches!` arms
//! scattered through a winit callback is a poor place to look up what a key
//! does.
//!
//! The config's `[keys]` section folds into the same table: a user entry on a
//! chord replaces the built-in binding on it, a new chord adds one, and
//! `"None"` unbinds. Everything downstream -- dispatch, the palette, the MCP
//! reply -- reads the folded result, so a rebound key cannot mean different
//! things in different places.

use unterm_engine::next_core::config::{Config, ConfigError};
use winit::keyboard::{Key, NamedKey};

/// Which way a pane-focus key points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Direction::Left => "Left",
            Direction::Right => "Right",
            Direction::Up => "Up",
            Direction::Down => "Down",
        }
    }
}

/// Something the front end does in response to a key, rather than sending it
/// to the shell.
///
/// Also the command palette's rows: an action a key can reach is an action a
/// palette should list, and keeping them one list is what stops the two from
/// drifting apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Copy,
    Paste,
    SplitRight,
    SplitDown,
    ScrollPageUp,
    ScrollPageDown,
    PreviousPrompt,
    NextPrompt,
    NewTab,
    NextTab,
    PreviousTab,
    CloseTab,
    Search,
    CommandPalette,
    Launcher,
    CopyMode,
    QuickSelect,
    /// A read-only card of what the AI layer is seeing and doing.
    Insights,
    CockpitInbox,
    GitPanel,
    Composer,
    ThemePicker,
    LeftTabBar,
    DirJump,
    NewWindow,
    /// A window whose shells run as administrator, through UAC. Windows only.
    NewAdminWindow,
    ClosePane,
    ZoomPane,
    /// Open the settings page in a browser.
    Settings,
    /// Find a character by its name and type it.
    CharSelect,
    /// Show the files under the pane's directory, down the left edge.
    TreeSidebar,
    /// Send a crew of agents at one task, each in its own worktree.
    FleetLaunch,
    /// Throw away the scrollback, keeping what is on screen.
    ClearScrollback,
    /// Throw away the scrollback and the screen with it.
    ClearScreen,
    /// Put a letter on every pane and go to the one that is typed.
    SelectPane,
    /// The same, but exchange the chosen pane with the one in front.
    SwapPane,
    FocusPane(Direction),
    ResizePane(Direction),
    MoveTab(isize),
    /// One-based, as the key is labelled.
    SelectTab(u8),
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    ToggleFullScreen,
}

impl Action {
    /// The name an agent sees. Kept stable: it is part of the MCP reply.
    pub fn name(self) -> &'static str {
        match self {
            Action::Copy => "Copy",
            Action::Paste => "Paste",
            Action::SplitRight => "SplitRight",
            Action::SplitDown => "SplitDown",
            Action::ScrollPageUp => "ScrollPageUp",
            Action::ScrollPageDown => "ScrollPageDown",
            Action::PreviousPrompt => "PreviousPrompt",
            Action::NextPrompt => "NextPrompt",
            Action::NewTab => "NewTab",
            Action::NextTab => "NextTab",
            Action::PreviousTab => "PreviousTab",
            Action::CloseTab => "CloseTab",
            Action::Search => "Search",
            Action::CommandPalette => "CommandPalette",
            Action::Launcher => "Launcher",
            Action::CopyMode => "CopyMode",
            Action::QuickSelect => "QuickSelect",
            Action::Insights => "Insights",
            Action::CockpitInbox => "CockpitInbox",
            Action::GitPanel => "GitPanel",
            Action::Composer => "Composer",
            Action::ThemePicker => "ThemePicker",
            Action::LeftTabBar => "LeftTabBar",
            Action::DirJump => "DirJump",
            Action::NewWindow => "NewWindow",
            Action::NewAdminWindow => "NewAdminWindow",
            Action::ClosePane => "ClosePane",
            Action::ZoomPane => "ZoomPane",
            Action::Settings => "Settings",
            Action::CharSelect => "CharSelect",
            Action::TreeSidebar => "TreeSidebar",
            Action::FleetLaunch => "FleetLaunch",
            Action::ClearScrollback => "ClearScrollback",
            Action::ClearScreen => "ClearScreen",
            Action::SelectPane => "SelectPane",
            Action::SwapPane => "SwapPane",
            Action::FocusPane(Direction::Left) => "FocusPaneLeft",
            Action::FocusPane(Direction::Right) => "FocusPaneRight",
            Action::FocusPane(Direction::Up) => "FocusPaneUp",
            Action::FocusPane(Direction::Down) => "FocusPaneDown",
            Action::ResizePane(Direction::Left) => "ResizePaneLeft",
            Action::ResizePane(Direction::Right) => "ResizePaneRight",
            Action::ResizePane(Direction::Up) => "ResizePaneUp",
            Action::ResizePane(Direction::Down) => "ResizePaneDown",
            Action::MoveTab(step) if step < 0 => "MoveTabLeft",
            Action::MoveTab(_) => "MoveTabRight",
            Action::SelectTab(1) => "SelectTab1",
            Action::SelectTab(2) => "SelectTab2",
            Action::SelectTab(3) => "SelectTab3",
            Action::SelectTab(4) => "SelectTab4",
            Action::SelectTab(5) => "SelectTab5",
            Action::SelectTab(6) => "SelectTab6",
            Action::SelectTab(7) => "SelectTab7",
            Action::SelectTab(8) => "SelectTab8",
            Action::SelectTab(_) => "SelectTab9",
            Action::IncreaseFontSize => "IncreaseFontSize",
            Action::DecreaseFontSize => "DecreaseFontSize",
            Action::ResetFontSize => "ResetFontSize",
            Action::ToggleFullScreen => "ToggleFullScreen",
        }
    }
}

impl Action {
    /// What the palette calls this.
    ///
    /// Separate from `name()`, which is the stable identifier an agent sees:
    /// renaming a row in the UI should not change what a script matches on.
    pub fn label(self) -> &'static str {
        match self {
            Action::Copy => "Copy",
            Action::Paste => "Paste",
            Action::SplitRight => "Split Right",
            Action::SplitDown => "Split Down",
            Action::ScrollPageUp => "Scroll Page Up",
            Action::ScrollPageDown => "Scroll Page Down",
            Action::PreviousPrompt => "Previous Prompt",
            Action::NextPrompt => "Next Prompt",
            Action::NewTab => "New Tab",
            Action::NextTab => "Next Tab",
            Action::PreviousTab => "Previous Tab",
            Action::CloseTab => "Close Tab",
            Action::Search => "Search",
            Action::CommandPalette => "Command Palette",
            Action::Launcher => "New Tab With...",
            Action::CopyMode => "Copy Mode",
            Action::QuickSelect => "Quick Select",
            Action::Insights => "Insights",
            Action::CockpitInbox => "Agent Inbox",
            Action::GitPanel => "Git Status",
            Action::Composer => "Prompt Queue",
            Action::ThemePicker => "Theme",
            Action::LeftTabBar => "Left Tab Bar",
            Action::DirJump => "Go to Directory",
            Action::NewWindow => "New Window",
            Action::NewAdminWindow => "New Administrator Window",
            Action::ClosePane => "Close Pane",
            Action::ZoomPane => "Zoom Pane",
            Action::Settings => "Settings",
            Action::CharSelect => "Insert Character",
            Action::TreeSidebar => "File Tree",
            Action::FleetLaunch => "Launch Fleet",
            Action::ClearScrollback => "Clear Scrollback",
            Action::ClearScreen => "Clear Screen",
            Action::SelectPane => "Select Pane",
            Action::SwapPane => "Swap Pane",
            Action::FocusPane(Direction::Left) => "Focus Pane Left",
            Action::FocusPane(Direction::Right) => "Focus Pane Right",
            Action::FocusPane(Direction::Up) => "Focus Pane Up",
            Action::FocusPane(Direction::Down) => "Focus Pane Down",
            Action::ResizePane(Direction::Left) => "Resize Pane Left",
            Action::ResizePane(Direction::Right) => "Resize Pane Right",
            Action::ResizePane(Direction::Up) => "Resize Pane Up",
            Action::ResizePane(Direction::Down) => "Resize Pane Down",
            Action::MoveTab(step) if step < 0 => "Move Tab Left",
            Action::MoveTab(_) => "Move Tab Right",
            // One row for the whole family: nine that differ by a digit push
            // everything else off a short list.
            Action::SelectTab(_) => "Select Tab By Number",
            Action::IncreaseFontSize => "Increase Font Size",
            Action::DecreaseFontSize => "Decrease Font Size",
            Action::ResetFontSize => "Reset Font Size",
            Action::ToggleFullScreen => "Full Screen",
        }
    }
}

/// Which modifiers a binding needs held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    /// Alt is a modifier the shell wants too -- Alt+B and Alt+F are readline's
    /// word motions -- so it appears here on arrow keys and nothing else.
    pub alt: bool,
}

impl Mods {
    pub fn name(self) -> &'static str {
        match (self.ctrl, self.shift, self.alt) {
            (true, true, false) => "CTRL|SHIFT",
            (true, false, false) => "CTRL",
            (false, true, false) => "SHIFT",
            (false, false, true) => "ALT",
            (true, false, true) => "CTRL|ALT",
            (false, true, true) => "SHIFT|ALT",
            (true, true, true) => "CTRL|SHIFT|ALT",
            (false, false, false) => "NONE",
        }
    }
}

/// The key a binding is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// A character, matched without regard to case: Ctrl+Shift+C is the same
    /// binding whether or not the shift also capitalised the letter.
    Char(char),
    PageUp,
    PageDown,
    Tab,
    Arrow(Direction),
    /// F1 and up.
    Function(u8),
}

impl Trigger {
    pub fn name(self) -> String {
        match self {
            Trigger::Char(c) => c.to_ascii_uppercase().to_string(),
            Trigger::PageUp => "PageUp".to_string(),
            Trigger::PageDown => "PageDown".to_string(),
            Trigger::Tab => "Tab".to_string(),
            Trigger::Arrow(direction) => direction.name().to_string(),
            Trigger::Function(number) => format!("F{number}"),
        }
    }

    fn matches(self, key: &Key) -> bool {
        match (self, key) {
            (Trigger::Char(c), Key::Character(text)) => {
                text.eq_ignore_ascii_case(c.encode_utf8(&mut [0u8; 4]))
            }
            (Trigger::PageUp, Key::Named(NamedKey::PageUp)) => true,
            (Trigger::PageDown, Key::Named(NamedKey::PageDown)) => true,
            (Trigger::Tab, Key::Named(NamedKey::Tab)) => true,
            (Trigger::Arrow(Direction::Left), Key::Named(NamedKey::ArrowLeft)) => true,
            (Trigger::Arrow(Direction::Right), Key::Named(NamedKey::ArrowRight)) => true,
            (Trigger::Arrow(Direction::Up), Key::Named(NamedKey::ArrowUp)) => true,
            (Trigger::Arrow(Direction::Down), Key::Named(NamedKey::ArrowDown)) => true,
            (Trigger::Function(number), Key::Named(named)) => {
                function_number(*named) == Some(number)
            }
            _ => false,
        }
    }
}

/// Which function key this is, if it is one.
fn function_number(named: NamedKey) -> Option<u8> {
    Some(match named {
        NamedKey::F1 => 1,
        NamedKey::F2 => 2,
        NamedKey::F3 => 3,
        NamedKey::F4 => 4,
        NamedKey::F5 => 5,
        NamedKey::F6 => 6,
        NamedKey::F7 => 7,
        NamedKey::F8 => 8,
        NamedKey::F9 => 9,
        NamedKey::F10 => 10,
        NamedKey::F11 => 11,
        NamedKey::F12 => 12,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub mods: Mods,
    pub trigger: Trigger,
    pub action: Action,
}

const CTRL_SHIFT: Mods = Mods {
    ctrl: true,
    shift: true,
    alt: false,
};
const CTRL_SHIFT_ALT: Mods = Mods {
    ctrl: true,
    shift: true,
    alt: true,
};
const CTRL: Mods = Mods {
    ctrl: true,
    shift: false,
    alt: false,
};
const SHIFT: Mods = Mods {
    ctrl: false,
    shift: true,
    alt: false,
};
const ALT: Mods = Mods {
    ctrl: false,
    shift: false,
    alt: true,
};
const NONE: Mods = Mods {
    ctrl: false,
    shift: false,
    alt: false,
};

/// Every key this front end keeps for itself.
///
/// Everything not listed goes to the shell, which is why plain Ctrl+C is
/// absent: it has to stay interrupt or a running program can never be
/// stopped. Shift+Page scrolls the viewport for the same reason in reverse --
/// unshifted pages belong to the program, which is how a pager gets its keys.
pub const BINDINGS: &[Binding] = &[
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('c'),
        action: Action::Copy,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('v'),
        action: Action::Paste,
    },
    // Right and down, as every terminal spells them.
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('d'),
        action: Action::SplitRight,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('e'),
        action: Action::SplitDown,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('t'),
        action: Action::NewTab,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('w'),
        action: Action::CloseTab,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('f'),
        action: Action::Search,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('p'),
        action: Action::CommandPalette,
    },
    // Keep both the current launcher chord and the 0.57 chord. Existing users
    // should not lose the shell selector merely because next-core also gained
    // a multi-window action.
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('l'),
        action: Action::Launcher,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('n'),
        action: Action::Launcher,
    },
    Binding {
        mods: CTRL_SHIFT_ALT,
        trigger: Trigger::Char('n'),
        action: Action::NewWindow,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('z'),
        action: Action::ZoomPane,
    },
    // The pane selector, on the letter the previous front end used. Swapping
    // is the same gesture with a different ending, so it is the same key with
    // a different hand on it -- and Alt rather than another letter, because
    // the two belong together in the muscle memory.
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('\''),
        action: Action::SelectPane,
    },
    Binding {
        mods: CTRL_SHIFT_ALT,
        trigger: Trigger::Char('\''),
        action: Action::SwapPane,
    },
    // Throw away the history. The screen is kept, because the reason to ask is
    // almost always "this pane has a hundred thousand lines of build output
    // behind it" and not "I want to lose what I am reading". Adding Alt takes
    // the screen too, which is `clear` with nothing left to scroll back to.
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('k'),
        action: Action::ClearScrollback,
    },
    // The file tree, on the letter every editor uses for it.
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('b'),
        action: Action::TreeSidebar,
    },
    // The character picker, on the letter the previous front end used.
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('u'),
        action: Action::CharSelect,
    },
    // A crew of agents on one task, each in its own worktree. On the letter
    // the cockpit already uses for itself.
    Binding {
        mods: CTRL_SHIFT_ALT,
        trigger: Trigger::Char('a'),
        action: Action::FleetLaunch,
    },
    Binding {
        mods: CTRL_SHIFT_ALT,
        trigger: Trigger::Char('k'),
        action: Action::ClearScreen,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('q'),
        action: Action::ClosePane,
    },
    // Alt and an arrow moves between panes. Alt+letters stay with the shell,
    // which is where readline's word motions live.
    Binding {
        mods: ALT,
        trigger: Trigger::Arrow(Direction::Left),
        action: Action::FocusPane(Direction::Left),
    },
    Binding {
        mods: ALT,
        trigger: Trigger::Arrow(Direction::Right),
        action: Action::FocusPane(Direction::Right),
    },
    Binding {
        mods: ALT,
        trigger: Trigger::Arrow(Direction::Up),
        action: Action::FocusPane(Direction::Up),
    },
    Binding {
        mods: ALT,
        trigger: Trigger::Arrow(Direction::Down),
        action: Action::FocusPane(Direction::Down),
    },
    // Add Shift to move the nearest split boundary on that axis.
    Binding {
        mods: Mods {
            ctrl: false,
            shift: true,
            alt: true,
        },
        trigger: Trigger::Arrow(Direction::Left),
        action: Action::ResizePane(Direction::Left),
    },
    Binding {
        mods: Mods {
            ctrl: false,
            shift: true,
            alt: true,
        },
        trigger: Trigger::Arrow(Direction::Right),
        action: Action::ResizePane(Direction::Right),
    },
    Binding {
        mods: Mods {
            ctrl: false,
            shift: true,
            alt: true,
        },
        trigger: Trigger::Arrow(Direction::Up),
        action: Action::ResizePane(Direction::Up),
    },
    Binding {
        mods: Mods {
            ctrl: false,
            shift: true,
            alt: true,
        },
        trigger: Trigger::Arrow(Direction::Down),
        action: Action::ResizePane(Direction::Down),
    },
    // Shell integration marks prompts with OSC 133. These jump between those
    // semantic rows without colliding with Alt+arrow pane navigation.
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Arrow(Direction::Up),
        action: Action::PreviousPrompt,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Arrow(Direction::Down),
        action: Action::NextPrompt,
    },
    // Font size, on the keys every application uses. `=` rather than `+`
    // because that is the unshifted key, and a terminal that needed shift to
    // grow the text would be the only one.
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('='),
        action: Action::IncreaseFontSize,
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('+'),
        action: Action::IncreaseFontSize,
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('-'),
        action: Action::DecreaseFontSize,
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('0'),
        action: Action::ResetFontSize,
    },
    Binding {
        mods: NONE,
        trigger: Trigger::Function(11),
        action: Action::ToggleFullScreen,
    },
    // Ctrl and a digit goes to that tab. Nine is the last one, and reaches
    // the last tab however many there are, which is what every browser does.
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('1'),
        action: Action::SelectTab(1),
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('2'),
        action: Action::SelectTab(2),
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('3'),
        action: Action::SelectTab(3),
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('4'),
        action: Action::SelectTab(4),
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('5'),
        action: Action::SelectTab(5),
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('6'),
        action: Action::SelectTab(6),
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('7'),
        action: Action::SelectTab(7),
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('8'),
        action: Action::SelectTab(8),
    },
    Binding {
        mods: CTRL,
        trigger: Trigger::Char('9'),
        action: Action::SelectTab(9),
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('x'),
        action: Action::CopyMode,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('s'),
        action: Action::QuickSelect,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('a'),
        action: Action::CockpitInbox,
    },
    // The Insights overlay, on the letter 0.57.4 used for it.
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('i'),
        action: Action::Insights,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('g'),
        action: Action::GitPanel,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Char('j'),
        action: Action::Composer,
    },
    // Ctrl+Tab cycles, Ctrl+Shift+Tab cycles back -- the pair every tabbed
    // application uses. Plain Tab still belongs to the shell's completion.
    Binding {
        mods: CTRL,
        trigger: Trigger::Tab,
        action: Action::NextTab,
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::Tab,
        action: Action::PreviousTab,
    },
    Binding {
        mods: SHIFT,
        trigger: Trigger::PageUp,
        action: Action::ScrollPageUp,
    },
    Binding {
        mods: SHIFT,
        trigger: Trigger::PageDown,
        action: Action::ScrollPageDown,
    },
    // Reorder tabs without changing which stable tab id is active.
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::PageUp,
        action: Action::MoveTab(-1),
    },
    Binding {
        mods: CTRL_SHIFT,
        trigger: Trigger::PageDown,
        action: Action::MoveTab(1),
    },
];

/// A binding the config wrote. `action: None` is `"None"`: the chord is
/// taken away from the front end and falls through to the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserBinding {
    pub mods: Mods,
    pub trigger: Trigger,
    pub action: Option<Action>,
}

/// Every action a `[keys]` entry can name.
///
/// The same stable names `Action::name` reports over MCP, so what an agent
/// reads from `meta.keybindings` is exactly what a config writes -- and the
/// round trip is tested, which is what keeps the two from drifting.
const NAMED_ACTIONS: &[(&str, Action)] = &[
    ("Copy", Action::Copy),
    ("Paste", Action::Paste),
    ("SplitRight", Action::SplitRight),
    ("SplitDown", Action::SplitDown),
    ("ScrollPageUp", Action::ScrollPageUp),
    ("ScrollPageDown", Action::ScrollPageDown),
    ("PreviousPrompt", Action::PreviousPrompt),
    ("NextPrompt", Action::NextPrompt),
    ("NewTab", Action::NewTab),
    ("NextTab", Action::NextTab),
    ("PreviousTab", Action::PreviousTab),
    ("CloseTab", Action::CloseTab),
    ("Search", Action::Search),
    ("CommandPalette", Action::CommandPalette),
    ("Launcher", Action::Launcher),
    ("CopyMode", Action::CopyMode),
    ("QuickSelect", Action::QuickSelect),
    ("Insights", Action::Insights),
    ("CockpitInbox", Action::CockpitInbox),
    ("GitPanel", Action::GitPanel),
    ("Composer", Action::Composer),
    ("ThemePicker", Action::ThemePicker),
    ("LeftTabBar", Action::LeftTabBar),
    ("DirJump", Action::DirJump),
    ("NewWindow", Action::NewWindow),
    ("NewAdminWindow", Action::NewAdminWindow),
    ("ClosePane", Action::ClosePane),
    ("ZoomPane", Action::ZoomPane),
    ("Settings", Action::Settings),
    ("CharSelect", Action::CharSelect),
    ("TreeSidebar", Action::TreeSidebar),
    ("FleetLaunch", Action::FleetLaunch),
    ("ClearScrollback", Action::ClearScrollback),
    ("ClearScreen", Action::ClearScreen),
    ("SelectPane", Action::SelectPane),
    ("SwapPane", Action::SwapPane),
    ("FocusPaneLeft", Action::FocusPane(Direction::Left)),
    ("FocusPaneRight", Action::FocusPane(Direction::Right)),
    ("FocusPaneUp", Action::FocusPane(Direction::Up)),
    ("FocusPaneDown", Action::FocusPane(Direction::Down)),
    ("ResizePaneLeft", Action::ResizePane(Direction::Left)),
    ("ResizePaneRight", Action::ResizePane(Direction::Right)),
    ("ResizePaneUp", Action::ResizePane(Direction::Up)),
    ("ResizePaneDown", Action::ResizePane(Direction::Down)),
    ("MoveTabLeft", Action::MoveTab(-1)),
    ("MoveTabRight", Action::MoveTab(1)),
    ("SelectTab1", Action::SelectTab(1)),
    ("SelectTab2", Action::SelectTab(2)),
    ("SelectTab3", Action::SelectTab(3)),
    ("SelectTab4", Action::SelectTab(4)),
    ("SelectTab5", Action::SelectTab(5)),
    ("SelectTab6", Action::SelectTab(6)),
    ("SelectTab7", Action::SelectTab(7)),
    ("SelectTab8", Action::SelectTab(8)),
    ("SelectTab9", Action::SelectTab(9)),
    ("IncreaseFontSize", Action::IncreaseFontSize),
    ("DecreaseFontSize", Action::DecreaseFontSize),
    ("ResetFontSize", Action::ResetFontSize),
    ("ToggleFullScreen", Action::ToggleFullScreen),
];

/// The action a config names. Case-insensitive, because `newtab` for `NewTab`
/// is a spelling, not a different request.
pub fn action_by_name(name: &str) -> Option<Action> {
    NAMED_ACTIONS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
        .map(|(_, action)| *action)
}

/// A chord as the config spells one: modifiers with `|` or `+` between them
/// and the key last -- `CTRL|SHIFT+T`, `ALT+Left`, `F11`.
fn parse_chord(text: &str) -> Result<(Mods, Trigger), String> {
    let parts: Vec<&str> = text.split(['|', '+']).map(str::trim).collect();
    let (key, modifiers) = parts.split_last().expect("split always yields one part");
    let mut mods = NONE;
    for name in modifiers {
        match name.to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" => mods.ctrl = true,
            "SHIFT" => mods.shift = true,
            "ALT" | "OPT" | "META" => mods.alt = true,
            // `NONE+F11` is how `Mods::name` spells an unmodified chord, so
            // reading it back has to work.
            "NONE" => {}
            other => {
                return Err(format!(
                    "`{other}` is not a modifier -- CTRL, SHIFT and ALT are"
                ))
            }
        }
    }
    Ok((mods, parse_trigger(key)?))
}

fn parse_trigger(name: &str) -> Result<Trigger, String> {
    let mut chars = name.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        // Folded, as `Trigger::matches` folds: `CTRL+T` and `CTRL+t` are one
        // binding, and comparing triggers for an override must agree.
        return Ok(Trigger::Char(ch.to_ascii_lowercase()));
    }
    let folded = name.to_ascii_lowercase();
    if let Some(number) = folded
        .strip_prefix('f')
        .and_then(|digits| digits.parse::<u8>().ok())
    {
        if (1..=12).contains(&number) {
            return Ok(Trigger::Function(number));
        }
    }
    Ok(match folded.as_str() {
        "pageup" => Trigger::PageUp,
        "pagedown" => Trigger::PageDown,
        "tab" => Trigger::Tab,
        "left" => Trigger::Arrow(Direction::Left),
        "right" => Trigger::Arrow(Direction::Right),
        "up" => Trigger::Arrow(Direction::Up),
        "down" => Trigger::Arrow(Direction::Down),
        "space" => Trigger::Char(' '),
        // The keys the file format cannot spell directly: a chord ending in
        // `+=` would split at the wrong `=`, and `+` and `|` are separators.
        "equal" | "equals" => Trigger::Char('='),
        "plus" => Trigger::Char('+'),
        "minus" => Trigger::Char('-'),
        "" => return Err("the chord ends without a key".to_string()),
        _ => return Err(format!("`{name}` is not a key this terminal can bind")),
    })
}

/// The `[keys]` section: the user's bindings, and every complaint about them.
///
/// A broken entry is a warning and the entry is skipped, never a refusal to
/// start -- but each one is reported with its line, because a chord that
/// silently does nothing is the same failure as a setting that silently does
/// nothing.
pub fn user_bindings_from(config: &Config) -> (Vec<UserBinding>, Vec<ConfigError>) {
    let mut bindings = Vec::new();
    let mut errors = Vec::new();
    // The store hands keys back sorted; warnings should read in file order,
    // and a chord written twice should keep the later line's meaning.
    let mut chords: Vec<String> = config
        .keys()
        .filter_map(|key| key.strip_prefix("keys."))
        .map(String::from)
        .collect();
    chords.sort_by_key(|chord| config.line_of(&format!("keys.{chord}")).unwrap_or(0));

    for chord in chords {
        let key = format!("keys.{chord}");
        let line = config.line_of(&key).unwrap_or(0);
        let name = match config.str_of(&key) {
            Ok(Some(name)) => name.to_string(),
            Ok(None) => continue,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let (mods, trigger) = match parse_chord(&chord) {
            Ok(parsed) => parsed,
            Err(problem) => {
                errors.push(ConfigError {
                    line,
                    message: format!("`{chord}`: {problem}"),
                });
                continue;
            }
        };
        let action = if name.eq_ignore_ascii_case("none") {
            // Unbound: the chord goes back to the shell.
            None
        } else {
            match action_by_name(&name) {
                Some(action) => Some(action),
                None => {
                    errors.push(ConfigError {
                        line,
                        message: format!(
                            "`{name}` is not an action -- the command palette and \
                             `meta.keybindings` list the real names"
                        ),
                    });
                    continue;
                }
            }
        };
        bindings.push(UserBinding {
            mods,
            trigger,
            action,
        });
    }
    (bindings, errors)
}

/// The `[keys]` section, installed once at startup.
static USER_BINDINGS: std::sync::OnceLock<Vec<UserBinding>> = std::sync::OnceLock::new();

pub fn install_user_bindings(bindings: Vec<UserBinding>) {
    if USER_BINDINGS.set(bindings).is_err() {
        log::warn!("user key bindings were already installed");
    }
}

fn installed() -> &'static [UserBinding] {
    USER_BINDINGS.get().map(Vec::as_slice).unwrap_or(&[])
}

/// The bindings in force: the built-in table with the user's entries folded
/// in. What the palette lists and the MCP surface reports, so a rebound chord
/// shows its real meaning everywhere.
pub fn effective_bindings() -> Vec<Binding> {
    fold_bindings(installed())
}

fn fold_bindings(user: &[UserBinding]) -> Vec<Binding> {
    let mut bindings: Vec<Binding> = BINDINGS
        .iter()
        .filter(|binding| {
            !user
                .iter()
                .any(|entry| entry.mods == binding.mods && entry.trigger == binding.trigger)
        })
        .copied()
        .collect();
    bindings.extend(user.iter().filter_map(|entry| {
        entry.action.map(|action| Binding {
            mods: entry.mods,
            trigger: entry.trigger,
            action,
        })
    }));
    bindings
}

/// The chord an action is bound to, spelt the way the palette spells one.
pub fn chord_hint(action: Action) -> Option<String> {
    effective_bindings()
        .iter()
        .find(|binding| binding.action == action)
        .map(|binding| display_chord(binding.mods, binding.trigger))
}

/// Every distinct command the palette offers. Families whose members differ
/// by direction are all present; numbered tab selection is one row because
/// its label and purpose are shared.
/// Whether this platform can do `action` at all. The palette leaves out
/// what would only answer "not here".
pub fn available(action: Action) -> bool {
    match action {
        Action::NewAdminWindow => crate::admin::available(),
        _ => true,
    }
}

pub const PALETTE_ACTIONS: &[Action] = &[
    Action::Copy,
    Action::Paste,
    Action::SplitRight,
    Action::SplitDown,
    Action::ScrollPageUp,
    Action::ScrollPageDown,
    Action::PreviousPrompt,
    Action::NextPrompt,
    Action::NewTab,
    Action::NextTab,
    Action::PreviousTab,
    Action::CloseTab,
    Action::Search,
    Action::CommandPalette,
    Action::Launcher,
    Action::CopyMode,
    Action::QuickSelect,
    Action::Insights,
    Action::CockpitInbox,
    Action::GitPanel,
    Action::Composer,
    Action::ThemePicker,
    Action::LeftTabBar,
    Action::DirJump,
    Action::NewWindow,
    Action::NewAdminWindow,
    Action::ClosePane,
    Action::ZoomPane,
    Action::Settings,
    Action::CharSelect,
    Action::TreeSidebar,
    Action::FleetLaunch,
    Action::ClearScrollback,
    Action::ClearScreen,
    Action::SelectPane,
    Action::SwapPane,
    Action::FocusPane(Direction::Left),
    Action::FocusPane(Direction::Right),
    Action::FocusPane(Direction::Up),
    Action::FocusPane(Direction::Down),
    Action::ResizePane(Direction::Left),
    Action::ResizePane(Direction::Right),
    Action::ResizePane(Direction::Up),
    Action::ResizePane(Direction::Down),
    Action::MoveTab(-1),
    Action::MoveTab(1),
    Action::SelectTab(1),
    Action::IncreaseFontSize,
    Action::DecreaseFontSize,
    Action::ResetFontSize,
    Action::ToggleFullScreen,
];

/// A key chord as people expect to read it in menus.
///
/// `Mods::name` deliberately keeps the config-file spelling (`CTRL|SHIFT`),
/// but exposing that parser syntax in the command palette made shortcuts look
/// like broken labels. The UI uses platform-neutral title case and `+`.
pub fn display_chord(mods: Mods, trigger: Trigger) -> String {
    let mut parts = Vec::new();
    if mods.ctrl {
        parts.push("Ctrl".to_string());
    }
    if mods.shift {
        parts.push("Shift".to_string());
    }
    if mods.alt {
        parts.push("Alt".to_string());
    }
    parts.push(trigger.name());
    parts.join("+")
}

/// What this key press means to the front end, if anything.
///
/// The most specific binding wins: Ctrl+Shift+C is a copy, not a scroll that
/// happens to have shift held. The user's entries win over the built-ins,
/// which is what makes a `[keys]` line an override rather than a suggestion.
pub fn action_for(key: &Key, ctrl: bool, shift: bool, alt: bool) -> Option<Action> {
    action_in(installed(), key, ctrl, shift, alt)
}

fn action_in(
    user: &[UserBinding],
    key: &Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> Option<Action> {
    let mods = Mods { ctrl, shift, alt };
    if let Some(binding) = user
        .iter()
        .find(|binding| binding.mods == mods && binding.trigger.matches(key))
    {
        // `None` here is a deliberate unbinding, not a miss: the chord must
        // not fall through to the built-in it was written to disable.
        return binding.action;
    }
    BINDINGS
        .iter()
        .filter(|binding| {
            binding.mods.ctrl == ctrl && binding.mods.shift == shift && binding.mods.alt == alt
        })
        .find(|binding| binding.trigger.matches(key))
        .map(|binding| binding.action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    #[test]
    fn ctrl_shift_c_copies_whatever_the_shift_did_to_the_letter() {
        assert_eq!(
            action_for(&character("C"), true, true, false),
            Some(Action::Copy),
            "shift capitalised the letter, which is not a different key"
        );
        assert_eq!(
            action_for(&character("c"), true, true, false),
            Some(Action::Copy)
        );
    }

    #[test]
    fn plain_tab_still_completes_in_the_shell() {
        assert_eq!(
            action_for(&Key::Named(NamedKey::Tab), false, false, false),
            None,
            "taking Tab would break every shell's completion"
        );
        assert_eq!(
            action_for(&Key::Named(NamedKey::Tab), true, false, false),
            Some(Action::NextTab)
        );
        assert_eq!(
            action_for(&Key::Named(NamedKey::Tab), true, true, false),
            Some(Action::PreviousTab)
        );
    }

    #[test]
    fn plain_ctrl_c_stays_the_programs_interrupt() {
        assert_eq!(
            action_for(&character("c"), true, false, false),
            None,
            "taking Ctrl+C would leave no way to stop a running program"
        );
    }

    #[test]
    fn unshifted_pages_belong_to_the_program() {
        assert_eq!(
            action_for(&Key::Named(NamedKey::PageUp), false, false, false),
            None,
            "a pager needs its own PageUp"
        );
        assert_eq!(
            action_for(&Key::Named(NamedKey::PageUp), false, true, false),
            Some(Action::ScrollPageUp)
        );
    }

    #[test]
    fn every_binding_has_a_distinct_key() {
        for (i, a) in BINDINGS.iter().enumerate() {
            for b in &BINDINGS[i + 1..] {
                assert!(
                    !(a.mods == b.mods && a.trigger == b.trigger),
                    "{:?}+{:?} is bound twice",
                    a.mods,
                    a.trigger
                );
            }
        }
    }
}

#[cfg(test)]
mod added_binding_tests {
    use super::*;

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    /// Ctrl+= and Ctrl+- grow and shrink the text in every application that
    /// shows text. A terminal without them is the odd one out, and this one
    /// was: the whole family was missing.
    #[test]
    fn the_font_size_keys_exist() {
        assert_eq!(
            action_for(&character("="), true, false, false),
            Some(Action::IncreaseFontSize)
        );
        assert_eq!(
            action_for(&character("-"), true, false, false),
            Some(Action::DecreaseFontSize)
        );
        assert_eq!(
            action_for(&character("0"), true, false, false),
            Some(Action::ResetFontSize)
        );
    }

    /// On many layouts Ctrl+Shift+= is how `+` is typed, and people press it
    /// meaning "bigger" either way.
    #[test]
    fn the_shifted_plus_grows_the_text_too() {
        assert_eq!(
            action_for(&character("+"), true, false, false),
            Some(Action::IncreaseFontSize)
        );
    }

    #[test]
    fn ctrl_and_a_digit_goes_to_that_tab() {
        assert_eq!(
            action_for(&character("1"), true, false, false),
            Some(Action::SelectTab(1))
        );
        assert_eq!(
            action_for(&character("9"), true, false, false),
            Some(Action::SelectTab(9))
        );
    }

    /// Alt and an arrow moves between panes. Alt and a *letter* must not:
    /// Alt+B and Alt+F are readline's word motions, and taking them would
    /// break editing a long command line.
    #[test]
    fn alt_arrows_move_between_panes_and_alt_letters_do_not() {
        assert_eq!(
            action_for(&Key::Named(NamedKey::ArrowLeft), false, false, true),
            Some(Action::FocusPane(Direction::Left))
        );
        assert_eq!(
            action_for(&Key::Named(NamedKey::ArrowDown), false, false, true),
            Some(Action::FocusPane(Direction::Down))
        );
        assert_eq!(
            action_for(&character("b"), false, false, true),
            None,
            "Alt+B is readline's word-back"
        );
        assert_eq!(
            action_for(&character("f"), false, false, true),
            None,
            "Alt+F is readline's word-forward"
        );
    }

    #[test]
    fn pane_tab_and_prompt_navigation_have_distinct_shortcuts() {
        assert_eq!(
            action_for(&Key::Named(NamedKey::ArrowRight), false, true, true),
            Some(Action::ResizePane(Direction::Right))
        );
        assert_eq!(
            action_for(&Key::Named(NamedKey::PageUp), true, true, false),
            Some(Action::MoveTab(-1))
        );
        assert_eq!(
            action_for(&Key::Named(NamedKey::PageDown), true, true, false),
            Some(Action::MoveTab(1))
        );
        assert_eq!(
            action_for(&Key::Named(NamedKey::ArrowUp), true, true, false),
            Some(Action::PreviousPrompt)
        );
        assert_eq!(
            action_for(&Key::Named(NamedKey::ArrowDown), true, true, false),
            Some(Action::NextPrompt)
        );
    }

    /// A plain arrow moves the shell's cursor and must keep doing so.
    #[test]
    fn an_unmodified_arrow_still_belongs_to_the_shell() {
        assert_eq!(
            action_for(&Key::Named(NamedKey::ArrowLeft), false, false, false),
            None
        );
    }

    #[test]
    fn f11_goes_full_screen_and_the_other_function_keys_are_the_programs() {
        assert_eq!(
            action_for(&Key::Named(NamedKey::F11), false, false, false),
            Some(Action::ToggleFullScreen)
        );
        for key in [NamedKey::F1, NamedKey::F5, NamedKey::F10, NamedKey::F12] {
            assert_eq!(
                action_for(&Key::Named(key), false, false, false),
                None,
                "{key:?} belongs to whatever is running"
            );
        }
    }

    #[test]
    fn a_new_window_and_a_pane_of_ones_own_are_both_reachable() {
        assert_eq!(
            action_for(&character("n"), true, true, true),
            Some(Action::NewWindow)
        );
        assert_eq!(
            action_for(&character("q"), true, true, false),
            Some(Action::ClosePane)
        );
        assert_eq!(
            action_for(&character("z"), true, true, false),
            Some(Action::ZoomPane)
        );
        // Both the new chord and the old v0.57 chord reach the selector.
        assert_eq!(
            action_for(&character("l"), true, true, false),
            Some(Action::Launcher)
        );
        assert_eq!(
            action_for(&character("n"), true, true, false),
            Some(Action::Launcher)
        );
    }

    /// Every name `Action::name` reports can be written in a `[keys]` entry
    /// and comes back as the same action -- the whole point of keeping one
    /// list of names.
    #[test]
    fn every_built_in_action_can_be_named_in_the_config() {
        for binding in BINDINGS {
            assert_eq!(
                action_by_name(binding.action.name()),
                Some(binding.action),
                "{} does not round-trip",
                binding.action.name()
            );
        }
        for (name, action) in NAMED_ACTIONS {
            assert_eq!(action.name(), *name, "{name} is not the stable name");
        }
    }

    /// Every action has a name and a label, and no two *different* actions
    /// share a name -- the name is what an agent matches on over MCP.
    ///
    /// Two keys reaching one action is fine and deliberate: Ctrl+= and Ctrl++
    /// both grow the text, because on many layouts they are the same physical
    /// key with and without shift.
    #[test]
    fn every_bound_action_is_named_distinctly() {
        let mut by_name: std::collections::HashMap<&str, Action> = std::collections::HashMap::new();
        for binding in BINDINGS {
            let name = binding.action.name();
            assert!(!name.is_empty(), "{:?} has no name", binding.action);
            assert!(!binding.action.label().is_empty(), "{name} has no label");
            if let Some(other) = by_name.insert(name, binding.action) {
                assert_eq!(
                    other, binding.action,
                    "{other:?} and {:?} are both called {name}",
                    binding.action
                );
            }
        }
    }
}

#[cfg(test)]
mod user_binding_tests {
    use super::*;

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    fn parsed(source: &str) -> (Vec<UserBinding>, Vec<ConfigError>) {
        let config = unterm_engine::next_core::config::parse(source).expect("config should parse");
        user_bindings_from(&config)
    }

    fn bindings(source: &str) -> Vec<UserBinding> {
        let (bindings, errors) = parsed(source);
        assert!(errors.is_empty(), "{errors:?}");
        bindings
    }

    #[test]
    fn a_user_entry_on_a_built_in_chord_replaces_it() {
        let user = bindings("[keys]\nCTRL|SHIFT+T = \"Search\"");

        assert_eq!(
            action_in(&user, &character("t"), true, true, false),
            Some(Action::Search),
            "the user's meaning, not the built-in NewTab"
        );
        // The rest of the table is untouched.
        assert_eq!(
            action_in(&user, &character("c"), true, true, false),
            Some(Action::Copy)
        );
    }

    #[test]
    fn a_new_chord_adds_a_binding() {
        let user = bindings("[keys]\nCTRL|ALT+G = \"GitPanel\"");

        assert_eq!(
            action_in(&user, &character("g"), true, false, true),
            Some(Action::GitPanel)
        );
    }

    #[test]
    fn none_unbinds_a_built_in_chord() {
        let user = bindings("[keys]\nF11 = \"None\"");

        assert_eq!(
            action_in(&user, &Key::Named(NamedKey::F11), false, false, false),
            None,
            "the chord goes back to whatever is running"
        );
    }

    #[test]
    fn named_keys_and_aliases_parse() {
        let user = bindings(
            r#"
            [keys]
            SHIFT+PageUp = "MoveTabLeft"
            ALT+Left = "PreviousTab"
            CTRL+F5 = "Search"
            CTRL+Equal = "ResetFontSize"
            "#,
        );

        assert_eq!(user.len(), 4);
        assert_eq!(
            action_in(&user, &Key::Named(NamedKey::PageUp), false, true, false),
            Some(Action::MoveTab(-1))
        );
        assert_eq!(
            action_in(&user, &Key::Named(NamedKey::ArrowLeft), false, false, true),
            Some(Action::PreviousTab),
            "the user's entry beats the built-in FocusPaneLeft"
        );
        assert_eq!(
            action_in(&user, &Key::Named(NamedKey::F5), true, false, false),
            Some(Action::Search)
        );
        assert_eq!(
            action_in(&user, &character("="), true, false, false),
            Some(Action::ResetFontSize)
        );
    }

    #[test]
    fn the_chord_is_matched_without_regard_to_case() {
        // `CTRL+T` and `ctrl+shift+t` are spellings, not different chords.
        let user = bindings("[keys]\nctrl|shift+T = \"search\"");

        assert_eq!(
            action_in(&user, &character("T"), true, true, false),
            Some(Action::Search)
        );
    }

    #[test]
    fn an_unknown_action_is_a_warning_that_names_the_line() {
        let (user, errors) = parsed("[keys]\nCTRL|SHIFT+T = \"NewTabb\"");

        assert!(user.is_empty());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 2);
        assert!(
            errors[0].message.contains("NewTabb"),
            "{}",
            errors[0].message
        );
    }

    #[test]
    fn a_bad_chord_is_a_warning_not_a_binding() {
        let (user, errors) =
            parsed("[keys]\nHYPER+T = \"NewTab\"\nCTRL+Banana = \"NewTab\"\nCTRL+ = \"NewTab\"");

        assert!(user.is_empty());
        assert_eq!(errors.len(), 3);
        assert!(errors[0].message.contains("HYPER"), "{}", errors[0].message);
        assert!(
            errors[1].message.contains("Banana"),
            "{}",
            errors[1].message
        );
        assert!(
            errors[2].message.contains("without a key"),
            "{}",
            errors[2].message
        );
    }

    #[test]
    fn a_value_that_is_not_a_string_is_reported() {
        let (user, errors) = parsed("[keys]\nCTRL|SHIFT+T = 3");

        assert!(user.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].message.contains("string"),
            "{}",
            errors[0].message
        );
    }

    #[test]
    fn the_broken_entry_is_skipped_and_the_good_one_kept() {
        let (user, errors) = parsed("[keys]\nCTRL+Nope = \"NewTab\"\nCTRL|ALT+G = \"GitPanel\"");

        assert_eq!(user.len(), 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(user[0].action, Some(Action::GitPanel));
    }

    #[test]
    fn folded_bindings_show_the_override_not_the_original() {
        let user = bindings(
            "[keys]\nCTRL|SHIFT+T = \"Search\"\nF11 = \"None\"\nCTRL|ALT+G = \"GitPanel\"",
        );
        let folded = fold_bindings(&user);

        // One built-in replaced, one removed, one added.
        assert_eq!(folded.len(), BINDINGS.len() + 2 - 1 - 1);
        let on_t: Vec<Action> = folded
            .iter()
            .filter(|binding| binding.mods == CTRL_SHIFT && binding.trigger == Trigger::Char('t'))
            .map(|binding| binding.action)
            .collect();
        assert_eq!(on_t, vec![Action::Search]);
        assert!(
            !folded
                .iter()
                .any(|binding| binding.trigger == Trigger::Function(11)),
            "the unbound chord is not listed"
        );
        assert!(folded
            .iter()
            .any(|binding| binding.action == Action::GitPanel
                && binding.mods
                    == Mods {
                        ctrl: true,
                        shift: false,
                        alt: true
                    }));
    }

    #[test]
    fn with_no_user_entries_the_folded_table_is_the_built_in_one() {
        let folded = fold_bindings(&[]);
        assert_eq!(folded.len(), BINDINGS.len());
    }
}
