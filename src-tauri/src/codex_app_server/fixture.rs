// Standalone stdlib-only stdio peer compiled by the native transport tests.
use std::io::{BufRead, Write};

fn notify(method: &str, params: &str) {
    println!(r#"{{"method":"{method}","params":{params}}}"#);
    std::io::stdout().flush().unwrap();
}
fn complete(status: &str) {
    notify(
        "turn/completed",
        &format!(
            r#"{{"threadId":"thread-1","turn":{{"id":"turn-1","status":"{status}","error":{{"message":"test failure"}}}}}}"#
        ),
    );
}
fn main() {
    let scenario = std::env::var("SCENARIO").unwrap();
    let mut log = std::fs::File::create(std::env::var_os("RPC_LOG").unwrap()).unwrap();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        writeln!(log, "{line}").unwrap();
        log.flush().unwrap();
        if !line.contains("\"method\"") {
            if scenario == "approval" {
                complete("completed");
            }
            continue;
        }
        let Some(id_part) = line.split("\"id\":").nth(1) else {
            continue;
        };
        let id: String = id_part.chars().take_while(|c| c.is_ascii_digit()).collect();
        let result = if line.contains("\"initialize\"") {
            if scenario == "exit" {
                std::process::exit(7);
            }
            if scenario == "malformed" {
                println!("invalid json");
                continue;
            }
            if scenario == "hang" {
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
            "{}"
        } else if line.contains("\"thread/start\"") || line.contains("\"thread/resume\"") {
            r#"{"thread":{"id":"thread-1"}}"#
        } else if line.contains("\"thread/read\"") {
            r#"{"thread":{"turns":[{"id":"first"},{"id":"second"}]}}"#
        } else if line.contains("\"thread/fork\"") {
            r#"{"thread":{"id":"fork-1"}}"#
        } else if line.contains("\"turn/start\"") {
            notify(
                "turn/started",
                r#"{"threadId":"thread-1","turn":{"id":"turn-1"}}"#,
            );
            notify(
                "item/started",
                r#"{"threadId":"thread-1","item":{"id":"a","type":"agentMessage","text":""}}"#,
            );
            notify(
                "item/agentMessage/delta",
                r#"{"threadId":"thread-1","itemId":"a","delta":"hello "}"#,
            );
            notify(
                "item/agentMessage/delta",
                r#"{"threadId":"thread-1","itemId":"a","delta":"world"}"#,
            );
            notify(
                "item/completed",
                r#"{"threadId":"thread-1","item":{"id":"a","type":"agentMessage","text":"hello world","phase":"final_answer"}}"#,
            );
            notify(
                "item/reasoning/summaryTextDelta",
                r#"{"threadId":"thread-1","itemId":"r","summaryIndex":0,"delta":"thinking"}"#,
            );
            notify(
                "turn/plan/updated",
                r#"{"threadId":"thread-1","plan":[{"step":"verify","status":"inProgress"}]}"#,
            );
            notify(
                "thread/tokenUsage/updated",
                r#"{"threadId":"thread-1","tokenUsage":{"total":{"inputTokens":100,"outputTokens":20,"cachedInputTokens":30}}}"#,
            );
            match scenario.as_str() {
                "cancel" | "steer" => (),
                "approval" => println!(
                    r#"{{"id":3,"method":"item/commandExecution/requestApproval","params":{{"reason":"test approval"}}}}"#
                ),
                "failed" => complete("failed"),
                _ => complete("completed"), // deliberately precedes turn/start response
            }
            r#"{"turn":{"id":"turn-1"}}"#
        } else if line.contains("\"turn/interrupt\"") {
            complete("interrupted");
            "{}"
        } else if line.contains("\"turn/steer\"") {
            complete("completed");
            "{}"
        } else {
            "{}"
        };
        println!(r#"{{"id":{id},"result":{result}}}"#);
        std::io::stdout().flush().unwrap();
    }
}
