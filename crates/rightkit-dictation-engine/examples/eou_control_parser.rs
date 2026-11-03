//! JSONL adapter for exact shared control-command parsing used by Windows EOU evidence.

use std::io::{self, BufRead};

use rightkit_text_polish::text_pipeline::parse_control_command;
use serde::Deserialize;

#[derive(Deserialize)]
struct Request {
    text: String,
}

fn parse_line(text: &str) -> Result<String, serde_json::Error> {
    match parse_control_command(text) {
        Some(command) => serde_json::to_string(&serde_json::json!({
            "clean_text": command.clean_text,
            "intent": format!("{:?}", command.intent),
            "verb": command.verb,
            "wake_word": command.wake_word,
        })),
        None => Ok("null".to_string()),
    }
}

fn main() {
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { return };
        let response =
            serde_json::from_str::<Request>(&line).and_then(|request| parse_line(&request.text));
        match response {
            Ok(json) => println!("{json}"),
            Err(error) => println!(
                r#"{{"error":{}}}"#,
                serde_json::to_string(&error.to_string()).expect("serialize error")
            ),
        }
    }
}
