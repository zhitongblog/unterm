//! Which browser actions commit something.
//!
//! Working a page is ordinary: typing into a form, opening a menu or
//! following a link changes nothing anyone else sees. Clicking *Send*,
//! *Publish*, *Pay* or *Delete*, or pressing Enter in a message box, does,
//! and it cannot be called back. Those were judged like any click, so an
//! agent at the balanced level sent mail without anyone being asked.
//!
//! A click names its element by a snapshot reference (`ref=12`), which says
//! nothing by itself. What it points at is in the page the agent last read:
//! every snapshot it takes passes through the session on its way back, and
//! the labels are kept from there ([`PageMemory`]). An action whose target
//! cannot be read is judged as before; one that plainly commits is raised to
//! the band of what it commits ([`Commit`]).
//!
//! Preparing is never raised. Typing the message, filling the form and
//! opening the page are judged by their own effect; only the step that sends
//! is asked about, so the person answers once, about the thing that matters.

use std::collections::HashMap;

use serde_json::Value;
use unterm_gateway::Risk;

/// One element of the page the agent last read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub tag: String,
    /// Its visible name: the first line of the quoted text.
    pub label: String,
    /// `type=`, `name=`, `id=`, `placeholder=`, `role=`, `href=`.
    pub attrs: HashMap<String, String>,
}

/// The page the agent last read, by reference, and what it typed where.
#[derive(Clone, Debug, Default)]
pub struct PageMemory {
    pub url: String,
    pub title: String,
    refs: HashMap<String, Element>,
    /// The element last typed into and what was typed, so Enter can be
    /// told apart: in a search box it searches, in a message box it sends.
    typed: Option<(Option<Element>, String)>,
}

/// What a committing action does, for the person and for the gateway.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub risk: Risk,
    /// "Click “Send” on mail.google.com".
    pub what: String,
    /// The same action again: same page, same element, same words typed.
    /// A second one in a session is never answered unasked, because the
    /// first may already have gone out.
    pub fingerprint: String,
    /// The parts, for a screen that says it in the person's language:
    /// `click` (a control named `label`), `enter` (sending `text` from the
    /// field `label`), `form` or `script`; `site` is the page's host.
    pub kind: &'static str,
    pub label: String,
    pub site: String,
    pub text: Option<String>,
}

impl Commit {
    pub fn parts(&self) -> Value {
        serde_json::json!({
            "kind": self.kind,
            "label": self.label,
            "site": self.site,
            "text": self.text,
            "risk": self.risk.as_str(),
        })
    }
}

impl PageMemory {
    /// Keep the elements of a snapshot the agent just read. Anything else
    /// (a click's own result, a page's text) leaves the memory as it was.
    pub fn observe_result(&mut self, output: &str) {
        let Some(text) = snapshot_text(output) else { return };
        let (url, title) = page_header(&text);
        let refs = parse_refs(&text);
        if refs.is_empty() {
            return;
        }
        self.url = url;
        self.title = title;
        self.refs = refs;
    }

    /// Note what an action types, so a later Enter knows where it lands.
    pub fn observe_request(&mut self, tool: &str, input: &Value) {
        let name = bare_name(tool);
        if matches!(name, "browser_type" | "browser_input_text" | "page_type" | "human_type") {
            let target = selector_of(input).and_then(|s| self.element(&s));
            let text = ["text", "value", "content"]
                .iter()
                .find_map(|k| input.get(*k).and_then(Value::as_str))
                .unwrap_or_default()
                .to_string();
            self.typed = Some((target, text));
        }
    }

    pub fn element(&self, selector: &str) -> Option<Element> {
        let reference = selector.trim().strip_prefix("ref=").or_else(|| selector.trim().strip_prefix("ref:"))?;
        self.refs.get(reference.trim()).cloned()
    }

    fn site(&self) -> String {
        host_of(&self.url).unwrap_or_else(|| self.title.clone())
    }
}

/// Whether this browser action commits something, and what.
pub fn commit_of(tool: &str, input: &Value, page: &PageMemory) -> Option<Commit> {
    let name = bare_name(tool);
    let on = |what: String| {
        let site = page.site();
        if site.is_empty() {
            what
        } else {
            format!("{what} on {site}")
        }
    };
    match name {
        "browser_click" | "browser_double_click" | "page_click" | "human_click" => {
            let (label, element) = click_target(input, page)?;
            let risk = commit_word(&label, element.as_ref())?;
            Some(Commit {
                risk,
                what: on(format!("Click “{}”", clip(&label, 40))),
                fingerprint: fingerprint(name, page, &label),
                kind: "click",
                label: clip(&label, 40),
                site: page.site(),
                text: None,
            })
        }
        "browser_press_key" => {
            let key = input.get("key").and_then(Value::as_str).unwrap_or_default();
            if !key.eq_ignore_ascii_case("enter") {
                return None;
            }
            enter_commit(page)
        }
        "browser_type" | "browser_input_text" | "page_type" | "human_type" => {
            let submits = ["submit", "press_enter", "enter"]
                .iter()
                .any(|k| input.get(*k).and_then(Value::as_bool).unwrap_or(false))
                || ["text", "value"].iter().any(|k| {
                    input.get(*k).and_then(Value::as_str).is_some_and(|t| t.ends_with('\n'))
                });
            if !submits {
                return None;
            }
            let mut after = page.clone();
            after.observe_request(tool, input);
            enter_commit(&after)
        }
        "browser_fill_form" => {
            let submits = input.get("submit").and_then(Value::as_bool).unwrap_or(false);
            submits.then(|| Commit {
                risk: Risk::ExternalSideEffect,
                what: on("Fill in and submit a form".into()),
                fingerprint: fingerprint(name, page, &input["fields"].to_string()),
                kind: "form",
                label: String::new(),
                site: page.site(),
                text: None,
            })
        }
        "browser_evaluate" => {
            let script = ["expression", "script", "code", "function"]
                .iter()
                .find_map(|k| input.get(*k).and_then(Value::as_str))
                .unwrap_or_default();
            script_commits(script).then(|| Commit {
                risk: Risk::ExternalSideEffect,
                what: on("Run a script that clicks, submits or sends on the page".into()),
                fingerprint: fingerprint(name, page, script),
                kind: "script",
                label: String::new(),
                site: page.site(),
                text: None,
            })
        }
        "page_batch_actions" => {
            let mut page = page.clone();
            for action in input.get("actions").and_then(Value::as_array).into_iter().flatten() {
                let tool = action.get("tool").and_then(Value::as_str).unwrap_or_default();
                let arguments = action.get("arguments").cloned().unwrap_or(Value::Null);
                if let Some(commit) = commit_of(tool, &arguments, &page) {
                    return Some(commit);
                }
                page.observe_request(tool, &arguments);
            }
            None
        }
        _ => None,
    }
}

fn enter_commit(page: &PageMemory) -> Option<Commit> {
    let (element, text) = page.typed.as_ref()?;
    // Nothing known about where it was typed: Enter there is most often a
    // search, and asking about every one would teach the person to say yes
    // without reading.
    let element = element.as_ref()?;
    if is_search_box(element) || text.trim().is_empty() {
        return None;
    }
    let field = if element.label.is_empty() {
        element.attrs.get("placeholder").cloned().unwrap_or_else(|| element.tag.clone())
    } else {
        element.label.clone()
    };
    let site = page.site();
    let mut what = format!("Press Enter to send “{}” from {}", clip(text.trim(), 60), clip(&field, 30));
    if !site.is_empty() {
        what.push_str(&format!(" on {site}"));
    }
    Some(Commit {
        risk: Risk::ExternalSideEffect,
        what,
        fingerprint: fingerprint("enter", page, &format!("{field}|{text}")),
        kind: "enter",
        label: clip(&field, 30),
        site,
        text: Some(clip(text.trim(), 200)),
    })
}

fn click_target(input: &Value, page: &PageMemory) -> Option<(String, Option<Element>)> {
    if let Some(selector) = selector_of(input) {
        if let Some(element) = page.element(&selector) {
            return Some((element.label.clone(), Some(element)));
        }
        // `text=Send`, `role=button[name="Send"]`, a CSS id that says it.
        for prefix in ["text=", "label=", "aria-label=", "data-testid="] {
            if let Some(rest) = selector.strip_prefix(prefix) {
                return Some((rest.trim_matches(['"', '\'']).to_string(), None));
            }
        }
        if let Some(name) = quoted_name(&selector) {
            return Some((name, None));
        }
        let lowered = selector.to_ascii_lowercase();
        if lowered.contains("type=submit") || lowered.contains("type=\"submit\"") || lowered.contains("type='submit'") {
            return Some(("Submit".into(), None));
        }
        return Some((selector, None));
    }
    for key in ["name", "text"] {
        if let Some(text) = input.get(key).and_then(Value::as_str) {
            return Some((text.to_string(), None));
        }
    }
    None
}

fn selector_of(input: &Value) -> Option<String> {
    ["selector", "ref", "target", "element"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(str::to_string)
}

fn quoted_name(selector: &str) -> Option<String> {
    let at = selector.find("name=")?;
    let rest = selector[at + 5..].trim_start_matches(['"', '\'']);
    let end = rest.find(['"', '\'', ']']).unwrap_or(rest.len());
    Some(rest[..end].to_string()).filter(|s| !s.is_empty())
}

fn is_search_box(element: &Element) -> bool {
    let hay = [
        element.label.as_str(),
        element.attrs.get("type").map(String::as_str).unwrap_or_default(),
        element.attrs.get("name").map(String::as_str).unwrap_or_default(),
        element.attrs.get("id").map(String::as_str).unwrap_or_default(),
        element.attrs.get("placeholder").map(String::as_str).unwrap_or_default(),
        element.attrs.get("role").map(String::as_str).unwrap_or_default(),
    ]
    .join(" ")
    .to_lowercase();
    let name = element.attrs.get("name").map(String::as_str).unwrap_or_default();
    hay.contains("search") || hay.contains("搜索") || hay.contains("検索") || name == "q" || name == "wd" || name == "query"
}

/// Words on a control that commit something, by what they commit. English
/// is matched on the first word (or phrase) so "Send" and "Send now" count
/// and "Sent mail" or "Order history" do not; Chinese and Japanese, which
/// have no spaces, on a short label that starts with or is the word.
const MONEY_EN: &[&str] = &[
    "pay", "buy", "purchase", "checkout", "check out", "place order", "order now", "donate", "transfer", "subscribe",
    "upgrade", "top up", "complete purchase", "confirm payment", "confirm order",
];
const MONEY_CJK: &[&str] = &[
    "支付", "付款", "购买", "立即购买", "下单", "提交订单", "结算", "去结算", "充值", "转账", "打赏", "捐赠", "订阅", "开通",
    "購入", "注文", "支払",
];
const GONE_EN: &[&str] = &["delete", "remove", "discard", "empty trash", "close account", "deactivate", "erase", "wipe"];
const GONE_CJK: &[&str] = &["删除", "永久删除", "移除", "清空", "注销", "削除"];
const OUT_EN: &[&str] = &[
    "send", "submit", "post", "publish", "reply", "reply all", "forward", "share", "invite", "tweet", "retweet",
    "repost", "comment", "confirm", "approve", "merge", "sign", "book", "reserve", "apply", "upload", "schedule",
    "accept", "decline", "follow", "like", "go live", "release", "deploy",
];
const OUT_CJK: &[&str] = &[
    "发送", "发布", "提交", "回复", "转发", "分享", "邀请", "评论", "发表", "发帖", "确认", "同意", "批准", "合并", "签署",
    "预订", "预约", "申请", "上传", "关注", "点赞", "投递", "定时发布", "送信", "投稿", "公開",
];

fn commit_word(label: &str, element: Option<&Element>) -> Option<Risk> {
    let first_line = label.lines().next().unwrap_or_default().trim();
    if first_line.is_empty() {
        return None;
    }
    // A long link text is a sentence, not a control: "you can support us
    // by donating" is an article, not a payment.
    let is_link = element.is_some_and(|e| e.tag == "a");
    let max_chars = if is_link { 16 } else { 32 };
    if first_line.chars().count() > max_chars {
        return None;
    }
    let lowered = first_line.to_lowercase();
    let words: Vec<&str> = lowered.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let starts_with_phrase = |phrases: &[&str]| {
        phrases.iter().any(|phrase| {
            let parts: Vec<&str> = phrase.split(' ').collect();
            words.len() >= parts.len() && words[..parts.len()] == parts[..]
        })
    };
    let cjk = |list: &[&str]| {
        list.iter().any(|word| lowered.starts_with(word) || (lowered.chars().count() <= 8 && lowered.contains(word)))
    };
    if starts_with_phrase(MONEY_EN) || cjk(MONEY_CJK) {
        Some(Risk::Financial)
    } else if starts_with_phrase(GONE_EN) || cjk(GONE_CJK) {
        Some(Risk::Destructive)
    } else if starts_with_phrase(OUT_EN) || cjk(OUT_CJK) {
        Some(Risk::ExternalSideEffect)
    } else {
        None
    }
}

fn script_commits(script: &str) -> bool {
    let lowered = script.to_ascii_lowercase();
    let compact: String = lowered.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains(".click(")
        || compact.contains(".submit(")
        || compact.contains("requestsubmit(")
        || compact.contains("sendbeacon(")
        || compact.contains("dispatchevent(")
        || compact.contains("xmlhttprequest")
        || ["post", "put", "patch", "delete"].iter().any(|m| {
            compact.contains(&format!("method:'{m}'")) || compact.contains(&format!("method:\"{m}\"")) || compact.contains(&format!("method:`{m}`"))
        })
}

fn fingerprint(kind: &str, page: &PageMemory, what: &str) -> String {
    format!("{kind}|{}|{}", page.site(), what.trim())
}

/// `mcp__unzoo__browser_click`, `unzoo.browser_click`, `unzoo_browser_click`
/// and a batch's bare `browser_click` are the same tool.
fn bare_name(tool: &str) -> &str {
    if let Some((server, name)) = crate::tool_risk::mcp_parts(tool) {
        if server == "unzoo" {
            return name;
        }
    }
    tool
}

/// The snapshot's text: a tool result is the tool's text, often JSON with
/// the snapshot under `result`, sometimes behind a line that marks the page
/// as untrusted.
fn snapshot_text(output: &str) -> Option<String> {
    let from_json = output.find('{').and_then(|at| {
        let value: Value = serde_json::from_str(output[at..].trim()).ok()?;
        find_snapshot(&value)
    });
    let text = from_json.unwrap_or_else(|| output.to_string());
    let text = if !text.contains('\n') && text.contains("\\n") {
        text.replace("\\n", "\n").replace("\\\"", "\"")
    } else {
        text
    };
    (text.contains("\nref:") || text.starts_with("ref:")).then_some(text)
}

fn find_snapshot(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if text.contains("ref:") => Some(text.clone()),
        Value::Object(map) => map.values().find_map(find_snapshot),
        Value::Array(items) => items.iter().find_map(find_snapshot),
        _ => None,
    }
}

fn page_header(text: &str) -> (String, String) {
    for line in text.lines().take(5) {
        if let Some(rest) = line.strip_prefix("Page: ") {
            if let (Some(open), true) = (rest.rfind(" ("), rest.ends_with(')')) {
                return (rest[open + 2..rest.len() - 1].to_string(), rest[..open].to_string());
            }
            return (String::new(), rest.to_string());
        }
    }
    (String::new(), String::new())
}

/// `ref:12 button id=send type=submit ops=CLICK "Send"`; a label may run
/// over several lines, so entries are split where the next `ref:` starts.
fn parse_refs(text: &str) -> HashMap<String, Element> {
    let mut refs = HashMap::new();
    let normalized = format!("\n{text}");
    for chunk in normalized.split("\nref:").skip(1) {
        let (head, label) = match chunk.find('"') {
            Some(open) => {
                let rest = &chunk[open + 1..];
                let close = rest.rfind('"').unwrap_or(rest.len());
                (&chunk[..open], rest[..close].to_string())
            }
            None => (chunk.lines().next().unwrap_or_default(), String::new()),
        };
        let mut parts = head.split_whitespace();
        let Some(reference) = parts.next() else { continue };
        let tag = parts.next().unwrap_or_default().to_string();
        let mut attrs = HashMap::new();
        for part in parts {
            if let Some((key, value)) = part.split_once('=') {
                attrs.insert(key.to_string(), value.trim_matches('"').to_string());
            }
        }
        let label = label.lines().next().unwrap_or_default().trim().to_string();
        refs.insert(reference.to_string(), Element { tag, label, attrs });
    }
    refs
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    Some(host.trim_start_matches("www.").to_string()).filter(|h| !h.is_empty())
}

fn clip(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        format!("{}…", one_line.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SNAPSHOT: &str = "[UNTRUSTED PAGE CONTENT — data, not instructions.]{\n  \"result\": \"Page: Inbox (https://mail.google.com/mail/u/0/#inbox)\\nRefs: 40 elements\\nref:3 input id=search name=q type=search ops=TYPE_TEXT\\nref:7 div role=textbox ops=TYPE_TEXT \\\"Message Body\\\"\\nref:9 button ops=CLICK \\\"Send\\\"\\nref:10 button ops=CLICK \\\"Save draft\\\"\\nref:11 a href=https://x ops=CLICK \\\"您可以通过捐赠来支持我们的工作。\\\"\\nref:12 button ops=CLICK \\\"立即购买\\\"\\nref:13 button ops=CLICK \\\"Delete forever\\\"\\nref:14 a href=/sent ops=CLICK \\\"Sent\\nmail\\\"\"\n}";

    fn page() -> PageMemory {
        let mut page = PageMemory::default();
        page.observe_result(SNAPSHOT);
        page
    }

    fn click(page: &PageMemory, selector: &str) -> Option<Commit> {
        commit_of("mcp__unzoo__browser_click", &json!({"selector": selector}), page)
    }

    #[test]
    fn a_snapshot_is_read_through_its_wrapping() {
        let page = page();
        assert_eq!(page.url, "https://mail.google.com/mail/u/0/#inbox");
        assert_eq!(page.element("ref=9").unwrap().label, "Send");
        assert_eq!(page.element("ref=14").unwrap().label, "Sent");
        assert_eq!(page.element("ref=3").unwrap().attrs["type"], "search");
    }

    #[test]
    fn sending_paying_and_deleting_are_raised_and_the_rest_is_not() {
        let page = page();
        let send = click(&page, "ref=9").unwrap();
        assert_eq!(send.risk, Risk::ExternalSideEffect);
        assert_eq!(send.what, "Click “Send” on mail.google.com");
        assert_eq!(click(&page, "ref=12").unwrap().risk, Risk::Financial);
        assert_eq!(click(&page, "ref=13").unwrap().risk, Risk::Destructive);
        assert_eq!(click(&page, "ref=10"), None, "saving a draft sends nothing");
        assert_eq!(click(&page, "ref=14"), None, "the Sent folder is a link");
        assert_eq!(click(&page, "ref=11"), None, "a sentence is not a button");
        assert_eq!(click(&page, "ref=99"), None, "an unknown target is judged as before");
        assert_eq!(click(&page, "text=Publish").unwrap().risk, Risk::ExternalSideEffect);
        assert_eq!(click(&page, "button[type=submit]").unwrap().risk, Risk::ExternalSideEffect);
        assert_eq!(
            commit_of("unzoo_browser_click", &json!({"role": "button", "name": "Post"}), &page).unwrap().risk,
            Risk::ExternalSideEffect
        );
    }

    #[test]
    fn enter_sends_from_a_message_box_but_searches_from_a_search_box() {
        let mut page = page();
        let enter = json!({"key": "Enter"});
        page.observe_request("mcp__unzoo__browser_type", &json!({"selector": "ref=3", "text": "invoice"}));
        assert_eq!(commit_of("mcp__unzoo__browser_press_key", &enter, &page), None);
        page.observe_request("mcp__unzoo__browser_type", &json!({"selector": "ref=7", "text": "See you at 10"}));
        let sent = commit_of("mcp__unzoo__browser_press_key", &enter, &page).unwrap();
        assert!(sent.what.contains("See you at 10") && sent.what.contains("Message Body"), "{}", sent.what);
        assert_eq!(commit_of("mcp__unzoo__browser_press_key", &json!({"key": "Tab"}), &page), None);
        let typed_and_sent = commit_of(
            "mcp__unzoo__browser_type",
            &json!({"selector": "ref=7", "text": "hello", "submit": true}),
            &page,
        );
        assert!(typed_and_sent.is_some());
    }

    #[test]
    fn a_batch_or_a_script_that_sends_is_caught() {
        let page = page();
        let batch = json!({"actions": [
            {"tool": "browser_type", "arguments": {"selector": "ref=7", "text": "hi"}},
            {"tool": "browser_click", "arguments": {"selector": "ref=9"}},
        ]});
        assert_eq!(commit_of("mcp__unzoo__page_batch_actions", &batch, &page).unwrap().what, "Click “Send” on mail.google.com");
        assert!(commit_of("mcp__unzoo__browser_evaluate", &json!({"expression": "document.querySelector('form').submit()"}), &page).is_some());
        assert!(commit_of("mcp__unzoo__browser_evaluate", &json!({"expression": "fetch('/api', {method: 'POST'})"}), &page).is_some());
        assert!(commit_of("mcp__unzoo__browser_evaluate", &json!({"expression": "document.title"}), &page).is_none());
    }

    #[test]
    fn the_same_send_twice_has_the_same_fingerprint_and_another_message_does_not() {
        let mut page = page();
        page.observe_request("browser_type", &json!({"selector": "ref=7", "text": "one"}));
        let first = commit_of("browser_press_key", &json!({"key": "Enter"}), &page).unwrap();
        assert_eq!(first.fingerprint, commit_of("browser_press_key", &json!({"key": "Enter"}), &page).unwrap().fingerprint);
        page.observe_request("browser_type", &json!({"selector": "ref=7", "text": "two"}));
        assert_ne!(first.fingerprint, commit_of("browser_press_key", &json!({"key": "Enter"}), &page).unwrap().fingerprint);
    }
}
