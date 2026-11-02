pub mod tools;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use serde_json::{Value, json};

use tools::{Failure, Output};

pub const SUPPORTED: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

pub struct Server {
    config: Option<PathBuf>,
    logging: bool,
}

fn reply(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn failure(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub fn negotiate(asked: Option<&str>) -> &'static str {
    asked
        .and_then(|version| SUPPORTED.iter().find(|known| **known == version))
        .copied()
        .unwrap_or(SUPPORTED[0])
}

fn scrub(text: String) -> String {
    match std::env::var("GH_TOKEN") {
        Ok(token) if !token.trim().is_empty() => text.replace(token.trim(), "[redacted]"),
        _ => text,
    }
}

fn block(text: String) -> Value {
    json!({ "type": "text", "text": scrub(text) })
}

fn tool_result(outcome: Result<Output, Failure>) -> Value {
    match outcome {
        Ok(Output {
            text: Some(text),
            notes,
        }) => {
            let mut content = vec![block(text)];
            if !notes.is_empty() {
                content.push(block(notes.join("\n")));
            }
            json!({ "content": content, "isError": false })
        }
        Ok(Output { text: None, notes }) => {
            let message = if notes.is_empty() {
                "no result".to_string()
            } else {
                notes.join("\n")
            };
            json!({ "content": [block(message)], "isError": true })
        }
        Err(Failure::Invalid(message)) => {
            json!({ "content": [block(format!("invalid arguments: {message}"))], "isError": true })
        }
        Err(Failure::Failed(error)) => {
            json!({ "content": [block(format!("error: {error:#}"))], "isError": true })
        }
    }
}

impl Server {
    pub fn new(config: Option<PathBuf>) -> Self {
        let logging = crate::log::enabled(false, config.as_deref());
        Self { config, logging }
    }

    fn record(&self, name: &str, started: Instant, outcome: &Result<Output, Failure>) {
        if !self.logging {
            return;
        }
        let (exit, error) = match outcome {
            Ok(Output { text: Some(_), .. }) => (0, None),
            Ok(_) | Err(Failure::Invalid(_)) => (1, None),
            Err(Failure::Failed(error)) => (1, Some(crate::log::kind_of(error))),
        };
        let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let line = crate::log::build(
            &format!("mcp:{name}"),
            exit,
            error,
            millis,
            crate::log::stats::take(),
        );
        crate::log::write(&line);
    }

    fn call(&self, id: &Value, params: &Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return failure(id, INVALID_PARAMS, "tools/call needs a tool name");
        };
        if !tools::known(name) {
            return failure(id, INVALID_PARAMS, &format!("unknown tool `{name}`"));
        }
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(value @ Value::Object(_)) => value.clone(),
            Some(_) => return failure(id, INVALID_PARAMS, "arguments must be an object"),
        };
        crate::log::stats::take();
        let started = Instant::now();
        let outcome = tools::call(name, &args, self.config.clone());
        self.record(name, started, &outcome);
        reply(id, tool_result(outcome))
    }

    fn request(&self, id: &Value, method: &str, params: &Value) -> Value {
        match method {
            "initialize" => reply(
                id,
                json!({
                    "protocolVersion": negotiate(
                        params.get("protocolVersion").and_then(Value::as_str)
                    ),
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "pipetrain",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "instructions": "Read-only planner of PR merge order across repos. Nothing here writes to GitHub."
                }),
            ),
            "ping" => reply(id, json!({})),
            "tools/list" => reply(id, json!({ "tools": tools::definitions() })),
            "tools/call" => self.call(id, params),
            other => failure(id, METHOD_NOT_FOUND, &format!("method not found: {other}")),
        }
    }

    pub fn handle(&self, line: &str) -> Option<Value> {
        if line.trim().is_empty() {
            return None;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return Some(failure(&Value::Null, PARSE_ERROR, "parse error"));
        };
        let Some(object) = message.as_object() else {
            return Some(failure(
                &Value::Null,
                INVALID_REQUEST,
                "expected one JSON-RPC request object per line",
            ));
        };
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            if object.contains_key("result") || object.contains_key("error") {
                return None;
            }
            return object
                .get("id")
                .map(|id| failure(id, INVALID_REQUEST, "missing method"));
        };
        let id = object.get("id")?;
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        Some(self.request(id, method, &params))
    }
}

pub fn serve(config: Option<PathBuf>) -> Result<i32> {
    serve_with(
        &Server::new(config),
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    )
}

pub fn serve_with(server: &Server, input: impl BufRead, mut output: impl Write) -> Result<i32> {
    for line in input.lines() {
        let Ok(line) = line else {
            break;
        };
        if let Some(answer) = server.handle(&line) {
            writeln!(output, "{answer}")?;
            output.flush()?;
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        Server {
            config: None,
            logging: false,
        }
    }

    fn ask(line: &str) -> Value {
        server().handle(line).expect("an answer")
    }

    #[test]
    fn initialize_echoes_a_supported_version_and_falls_back_otherwise() {
        let known = ask(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        );
        assert_eq!(known["result"]["protocolVersion"], "2024-11-05");
        let unknown = ask(
            r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        assert_eq!(unknown["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(unknown["id"], 2);
        assert_eq!(unknown["result"]["serverInfo"]["name"], "pipetrain");
    }

    #[test]
    fn notifications_and_blank_lines_get_no_answer() {
        assert!(
            server()
                .handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none()
        );
        assert!(server().handle("  ").is_none());
        assert!(
            server()
                .handle(r#"{"jsonrpc":"2.0","method":"nope"}"#)
                .is_none()
        );
    }

    #[test]
    fn bad_input_gets_the_json_rpc_error_codes() {
        assert_eq!(ask("{not json")["error"]["code"], PARSE_ERROR);
        assert_eq!(ask("[1]")["error"]["code"], INVALID_REQUEST);
        assert_eq!(
            ask(r#"{"jsonrpc":"2.0","id":7,"method":"nope"}"#)["error"]["code"],
            METHOD_NOT_FOUND
        );
        assert_eq!(
            ask(r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"bundle"}}"#)["error"]
                ["code"],
            INVALID_PARAMS
        );
        assert_eq!(
            ask(r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{}}"#)["error"]["code"],
            INVALID_PARAMS
        );
    }

    #[test]
    fn the_tool_list_has_five_read_only_tools() {
        let answer = ask(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        let names: Vec<&str> = answer["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, tools::NAMES);
    }

    #[test]
    fn invalid_arguments_are_a_tool_error_not_a_crash() {
        let answer = ask(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"feed","arguments":{}}}"#,
        );
        assert_eq!(answer["result"]["isError"], true);
        assert!(
            answer["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("`package` is required")
        );
    }

    #[test]
    fn serve_writes_one_line_per_answer() {
        let input = "\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n{bad\n";
        let mut out = Vec::new();
        serve_with(&server(), input.as_bytes(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 2);
        for line in text.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
    }
}
