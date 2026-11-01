//! Small authenticated client for one peer Unterm instance.
//!
//! This is used by cross-window product actions, not by the public CLI. The
//! peer's endpoint and token come from the live instance registry.

use crate::server_info::InstanceInfo;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next_id: u64,
}

impl Client {
    fn connect(instance: &InstanceInfo) -> Result<Self> {
        if instance.mcp_port == 0 || instance.auth_token.is_empty() {
            return Err(anyhow!(
                "instance {} has no usable MCP endpoint",
                instance.id
            ));
        }
        let address: SocketAddr = format!("127.0.0.1:{}", instance.mcp_port).parse()?;
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
            .with_context(|| format!("connect to Unterm instance {}", instance.id))?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
        stream.set_nodelay(true).ok();
        let writer = stream.try_clone().context("clone peer MCP stream")?;
        let mut client = Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
        };
        let login = client.call("auth.login", json!({ "token": instance.auth_token }))?;
        if login.get("status").and_then(Value::as_str) != Some("ok") {
            return Err(anyhow!("peer MCP authentication was rejected"));
        }
        Ok(client)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        request.push('\n');
        self.writer.write_all(request.as_bytes())?;
        self.writer.flush()?;

        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(anyhow!("peer MCP connection closed during {method}"));
        }
        let response: Value = serde_json::from_str(line.trim())
            .with_context(|| format!("parse peer MCP response for {method}"))?;
        if let Some(error) = response.get("error") {
            return Err(anyhow!("peer MCP {method} failed: {error}"));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// Focus a pane inside a peer instance, then raise that instance's window.
pub fn focus_pane(instance: &InstanceInfo, pane_id: u64) -> Result<()> {
    let mut client = Client::connect(instance)?;
    client.call("session.focus", json!({ "id": pane_id }))?;
    client.call("instance.focus", json!({}))?;
    Ok(())
}

/// Execute a single authenticated JSON-RPC call against a peer Unterm instance.
pub fn call_instance(instance: &InstanceInfo, method: &str, params: Value) -> Result<Value> {
    let mut client = Client::connect(instance)?;
    client.call(method, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn peer_focus_authenticates_then_focuses_pane_and_window() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            for (method, expected_pane) in [
                ("auth.login", None),
                ("session.focus", Some(42_u64)),
                ("instance.focus", None),
            ] {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(line.trim()).unwrap();
                assert_eq!(request["method"], method);
                if method == "auth.login" {
                    assert_eq!(request["params"]["token"], "secret");
                }
                if let Some(pane) = expected_pane {
                    assert_eq!(request["params"]["id"], pane);
                }
                let result = if method == "auth.login" {
                    json!({ "status": "ok" })
                } else {
                    json!({ "ok": true })
                };
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": result,
                });
                writeln!(writer, "{}", serde_json::to_string(&response).unwrap()).unwrap();
                writer.flush().unwrap();
            }
        });
        let instance = InstanceInfo {
            id: "bravo".to_string(),
            mcp_port: port,
            auth_token: "secret".to_string(),
            ..Default::default()
        };

        focus_pane(&instance, 42).unwrap();
        server.join().unwrap();
    }
}
