//! Just enough IMAP to see new mail: sign in over TLS, look at the inbox
//! read-only (`EXAMINE`, `BODY.PEEK` -- nothing is marked read), and read
//! the sender, subject, date and the start of the text of messages that
//! came after the last one seen.
//!
//! The password is an app password (Gmail, iCloud, Yahoo) or an
//! authorization code (QQ, 163): kept in the system's credential store,
//! never in the task, never handed to the agent. The agent gets only what
//! the mail says, marked as data.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

/// One message, as the agent is told about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mail {
    pub uid: u32,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub text: String,
}

/// What one look at the inbox found.
#[derive(Debug, Default)]
pub struct Look {
    /// The inbox's UIDVALIDITY: a different one means its numbering started over.
    pub validity: u32,
    /// The highest message number seen.
    pub last_uid: u32,
    /// Messages after `after` (none on a first look), oldest first.
    pub new: Vec<Mail>,
}

const MAX_NEW: usize = 15;
const TEXT_BYTES: usize = 24 * 1024;
const MAX_TEXT_CHARS: usize = 1500;

/// The usual IMAP server for an address, so a person only types it for an
/// unusual one.
pub fn guess_host(address: &str) -> Option<&'static str> {
    let domain = address.rsplit_once('@')?.1.trim().to_ascii_lowercase();
    Some(match domain.as_str() {
        "gmail.com" | "googlemail.com" => "imap.gmail.com",
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" => "outlook.office365.com",
        "qq.com" | "foxmail.com" => "imap.qq.com",
        "163.com" => "imap.163.com",
        "126.com" => "imap.126.com",
        "yeah.net" => "imap.yeah.net",
        "sina.com" => "imap.sina.com",
        "icloud.com" | "me.com" | "mac.com" => "imap.mail.me.com",
        "yahoo.com" => "imap.mail.yahoo.com",
        "aliyun.com" => "imap.aliyun.com",
        _ => return None,
    })
}

enum Part {
    Text(String),
    Literal(Vec<u8>),
}

struct Session {
    stream: BufReader<native_tls::TlsStream<TcpStream>>,
    tag: u32,
}

impl Session {
    fn connect(host: &str, port: u16) -> Result<Self> {
        let address = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("cannot find the mail server {host}"))?
            .next()
            .ok_or_else(|| anyhow!("cannot find the mail server {host}"))?;
        let tcp = TcpStream::connect_timeout(&address, Duration::from_secs(15))
            .with_context(|| format!("cannot reach the mail server {host}:{port}"))?;
        tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(30)))?;
        let tls = native_tls::TlsConnector::new()?
            .connect(host, tcp)
            .map_err(|e| anyhow!("secure connection to {host} failed: {e}"))?;
        let mut session = Self { stream: BufReader::new(tls), tag: 0 };
        let greeting = session.line()?;
        if !greeting.starts_with("* OK") && !greeting.starts_with("* PREAUTH") {
            bail!("the mail server did not greet: {}", greeting.trim());
        }
        Ok(session)
    }

    fn line(&mut self) -> Result<String> {
        let mut bytes = Vec::new();
        let read = self.stream.read_until(b'\n', &mut bytes)?;
        if read == 0 {
            bail!("the mail server closed the connection");
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Send one command and read its answer: every untagged response, each
    /// as its text and literals in order, then the tagged status.
    fn command(&mut self, command: &str) -> Result<Vec<Vec<Part>>> {
        self.tag += 1;
        let tag = format!("a{}", self.tag);
        self.stream.get_mut().write_all(format!("{tag} {command}\r\n").as_bytes())?;
        let mut responses = Vec::new();
        let mut current: Vec<Part> = Vec::new();
        loop {
            let line = self.line()?;
            if line.starts_with(&format!("{tag} ")) {
                if !current.is_empty() {
                    responses.push(std::mem::take(&mut current));
                }
                let status = line[tag.len() + 1..].trim();
                if !status.starts_with("OK") {
                    bail!("{}", status.trim_start_matches("NO ").trim_start_matches("BAD "));
                }
                return Ok(responses);
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if let Some(size) = literal_size(trimmed) {
                current.push(Part::Text(trimmed.to_string()));
                let mut data = vec![0; size];
                self.stream.read_exact(&mut data)?;
                current.push(Part::Literal(data));
                continue;
            }
            current.push(Part::Text(trimmed.to_string()));
            // A line that does not end in a literal ends the response.
            responses.push(std::mem::take(&mut current));
        }
    }
}

fn literal_size(line: &str) -> Option<usize> {
    let open = line.strip_suffix('}')?.rfind('{')?;
    line[open + 1..line.len() - 1].trim_end_matches('+').parse().ok()
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn number_after(text: &str, key: &str) -> Option<u32> {
    let at = text.find(key)? + key.len();
    text[at..].trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// Look at the inbox. With `after` (the validity and last message seen),
/// return what came since; without, only where it stands now.
pub fn look(host: &str, port: u16, user: &str, password: &str, after: Option<(u32, u32)>) -> Result<Look> {
    let mut session = Session::connect(host, port)?;
    session
        .command(&format!("LOGIN {} {}", quoted(user), quoted(password)))
        .map_err(|e| anyhow!("the mail server refused the sign-in ({e}); use an app password or authorization code"))?;
    let examined = session.command("EXAMINE INBOX")?;
    let mut validity = 0;
    let mut next = None;
    for response in &examined {
        for part in response {
            if let Part::Text(text) = part {
                validity = number_after(text, "UIDVALIDITY").unwrap_or(validity);
                next = number_after(text, "UIDNEXT").or(next);
            }
        }
    }
    let mut look = Look { validity, ..Look::default() };
    let since = match after {
        Some((seen_validity, last)) if seen_validity == validity => last,
        _ => {
            // A first look, or the numbering started over: only where it stands.
            look.last_uid = match next {
                Some(next) => next.saturating_sub(1),
                None => newest_uid(&mut session)?,
            };
            let _ = session.command("LOGOUT");
            return Ok(look);
        }
    };
    look.last_uid = since;
    let mut uids: Vec<u32> = Vec::new();
    for response in session.command(&format!("UID SEARCH UID {}:*", since.saturating_add(1)))? {
        for part in response {
            if let Part::Text(text) = part {
                if let Some(list) = text.strip_prefix("* SEARCH") {
                    uids.extend(list.split_whitespace().filter_map(|n| n.parse::<u32>().ok()).filter(|n| *n > since));
                }
            }
        }
    }
    uids.sort_unstable();
    if let Some(newest) = uids.last() {
        look.last_uid = *newest;
    }
    let wanted: Vec<u32> = uids.iter().rev().take(MAX_NEW).rev().copied().collect();
    if !wanted.is_empty() {
        let list = wanted.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let fetched = session.command(&format!(
            "UID FETCH {list} (UID BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE CONTENT-TYPE CONTENT-TRANSFER-ENCODING)] BODY.PEEK[TEXT]<0.{TEXT_BYTES}>)"
        ))?;
        for response in fetched {
            if let Some(mail) = mail_of(&response) {
                look.new.push(mail);
            }
        }
        look.new.sort_by_key(|m| m.uid);
    }
    let _ = session.command("LOGOUT");
    Ok(look)
}

fn newest_uid(session: &mut Session) -> Result<u32> {
    let mut newest = 0;
    for response in session.command("UID SEARCH ALL")? {
        for part in response {
            if let Part::Text(text) = part {
                if let Some(list) = text.strip_prefix("* SEARCH") {
                    newest = list.split_whitespace().filter_map(|n| n.parse().ok()).max().unwrap_or(newest);
                }
            }
        }
    }
    Ok(newest)
}

fn mail_of(response: &[Part]) -> Option<Mail> {
    let mut uid = None;
    let mut header = None;
    let mut body = None;
    let mut before = String::new();
    for part in response {
        match part {
            Part::Text(text) => {
                uid = uid.or_else(|| number_after(text, "UID "));
                before = text.clone();
            }
            Part::Literal(data) => {
                if before.contains("HEADER") {
                    header = Some(data.clone());
                } else if before.contains("TEXT") {
                    body = Some(data.clone());
                }
            }
        }
    }
    let headers = parse_headers(&String::from_utf8_lossy(&header?));
    let get = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or_default();
    let text = body
        .map(|body| body_text(&get("content-type"), &get("content-transfer-encoding"), &body))
        .unwrap_or_default();
    Some(Mail {
        uid: uid?,
        from: decode_words(&get("from")),
        subject: decode_words(&get("subject")),
        date: get("date"),
        text: clip(&text, MAX_TEXT_CHARS),
    })
}

/// Header lines, unfolded, as (name, value).
fn parse_headers(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = out.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    out
}

fn param(header: &str, name: &str) -> Option<String> {
    header.split(';').skip(1).find_map(|part| {
        let (key, value) = part.split_once('=')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim().trim_matches('"').to_string())
    })
}

fn charset_decode(bytes: &[u8], charset: &str) -> String {
    let label = if charset.is_empty() { "utf-8" } else { charset };
    let encoding = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    encoding.decode(bytes).0.into_owned()
}

fn base64_decode(text: &str) -> Vec<u8> {
    use base64::Engine;
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(clean.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(clean.trim_end_matches('=')))
        .unwrap_or_default()
}

fn quoted_printable(text: &[u8], header: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        match text[i] {
            b'=' if text.get(i + 1) == Some(&b'\r') && text.get(i + 2) == Some(&b'\n') => i += 3,
            b'=' if text.get(i + 1) == Some(&b'\n') => i += 2,
            b'=' if i + 2 < text.len() => {
                match u8::from_str_radix(&String::from_utf8_lossy(&text[i + 1..i + 3]), 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'=');
                        i += 1;
                    }
                }
            }
            b'_' if header => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

/// `=?charset?B?…?=` and `=?charset?Q?…?=` words in a header, decoded.
pub(crate) fn decode_words(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let between = &rest[..start];
        let decoded = (|| {
            let inner = &rest[start + 2..];
            let (charset, inner) = inner.split_once('?')?;
            let (kind, inner) = inner.split_once('?')?;
            let end = inner.find("?=")?;
            let data = &inner[..end];
            let bytes = match kind.to_ascii_uppercase().as_str() {
                "B" => base64_decode(data),
                "Q" => quoted_printable(data.as_bytes(), true),
                _ => return None,
            };
            Some((charset_decode(&bytes, charset.split('*').next().unwrap_or(charset)), start + 2 + charset.len() + 1 + kind.len() + 1 + end + 2))
        })();
        match decoded {
            Some((text, used)) => {
                // Whitespace between two encoded words is not part of the text.
                if !(last_was_word && between.trim().is_empty()) {
                    out.push_str(between);
                }
                out.push_str(&text);
                rest = &rest[used..];
                last_was_word = true;
            }
            None => {
                out.push_str(&rest[..start + 2]);
                rest = &rest[start + 2..];
                last_was_word = false;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn decode_part(encoding: &str, charset: &str, data: &[u8]) -> String {
    let bytes = match encoding.trim().to_ascii_lowercase().as_str() {
        "base64" => base64_decode(&String::from_utf8_lossy(data)),
        "quoted-printable" => quoted_printable(data, false),
        _ => data.to_vec(),
    };
    charset_decode(&bytes, charset)
}

/// The readable text of a message body: its text/plain part, else its
/// text/html with the tags taken out. Best effort on a body cut short.
pub(crate) fn body_text(content_type: &str, encoding: &str, body: &[u8]) -> String {
    let kind = content_type.split(';').next().unwrap_or("text/plain").trim().to_ascii_lowercase();
    let charset = param(content_type, "charset").unwrap_or_default();
    if kind.starts_with("multipart/") {
        let Some(boundary) = param(content_type, "boundary") else { return String::new() };
        let text = String::from_utf8_lossy(body).into_owned();
        let mut html = None;
        for part in text.split(&format!("--{boundary}")).skip(1) {
            let part = part.trim_start_matches(['\r', '\n']);
            if part.starts_with("--") {
                break;
            }
            let (head, content) = part.split_once("\r\n\r\n").or_else(|| part.split_once("\n\n")).unwrap_or((part, ""));
            let headers = parse_headers(head);
            let get = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or_default();
            let part_type = get("content-type");
            let part_kind = part_type.split(';').next().unwrap_or("text/plain").trim().to_ascii_lowercase();
            if part_kind.starts_with("multipart/") || part_kind == "text/plain" {
                let found = body_text(if part_type.is_empty() { "text/plain" } else { &part_type }, &get("content-transfer-encoding"), content.as_bytes());
                if !found.trim().is_empty() {
                    return found;
                }
            } else if part_kind == "text/html" && html.is_none() {
                html = Some(body_text(&part_type, &get("content-transfer-encoding"), content.as_bytes()));
            }
        }
        return html.unwrap_or_default();
    }
    let text = decode_part(encoding, &charset, body);
    if kind == "text/html" {
        strip_html(&text)
    } else if kind.starts_with("text/") || kind.is_empty() {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        String::new()
    }
}

fn strip_html(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let lower = html.to_ascii_lowercase();
    let mut skip_until: Option<&str> = None;
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if let Some(end) = skip_until {
            match lower[i..].find(end) {
                Some(at) => {
                    i += at + end.len();
                    skip_until = None;
                    continue;
                }
                None => break,
            }
        }
        let c = bytes[i];
        if c == b'<' {
            if lower[i..].starts_with("<style") {
                skip_until = Some("</style>");
                continue;
            }
            if lower[i..].starts_with("<script") {
                skip_until = Some("</script>");
                continue;
            }
            in_tag = true;
            out.push(' ');
        } else if c == b'>' && in_tag {
            in_tag = false;
        } else if !in_tag {
            let ch_len = html[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&html[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        i += 1;
    }
    let out = out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_header_words_are_read_in_their_charset() {
        assert_eq!(decode_words("=?UTF-8?B?5Y+R56Wo?= 2026"), "发票 2026");
        assert_eq!(decode_words("=?gb2312?B?w7/W3MfltaU=?="), "每周清单");
        assert_eq!(decode_words("=?utf-8?Q?Caf=C3=A9_menu?="), "Café menu");
        assert_eq!(decode_words("=?UTF-8?B?5Y+R?= =?UTF-8?B?56Wo?="), "发票", "no space between adjacent words");
        assert_eq!(decode_words("Plain subject"), "Plain subject");
    }

    #[test]
    fn a_body_is_its_plain_text_part_else_its_html_without_tags() {
        let body = b"--b1\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n5L2g5aW977yM6K+35p+l5pS25Y+R56Wo44CC\r\n--b1\r\nContent-Type: text/html\r\n\r\n<p>x</p>\r\n--b1--\r\n";
        assert_eq!(body_text("multipart/alternative; boundary=\"b1\"", "", body), "你好，请查收发票。");
        let html = b"--b2\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n<style>p{}</style><p>Total: =E2=82=AC12</p><br>Thanks\r\n--b2--";
        assert_eq!(body_text("multipart/mixed; boundary=b2", "", html), "Total: €12 Thanks");
        assert_eq!(body_text("text/plain", "7bit", b"line one\r\nline two"), "line one line two");
    }

    #[test]
    fn literals_and_numbers_are_found_in_responses() {
        assert_eq!(literal_size("* 1 FETCH (UID 7 BODY[TEXT]<0> {123}"), Some(123));
        assert_eq!(literal_size("* OK done"), None);
        assert_eq!(number_after("* OK [UIDVALIDITY 1700] UIDs valid", "UIDVALIDITY"), Some(1700));
        assert_eq!(guess_host("someone@qq.com"), Some("imap.qq.com"));
        assert_eq!(guess_host("a@example.org"), None);
    }
}
