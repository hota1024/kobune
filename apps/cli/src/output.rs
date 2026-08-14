//! Output that is not meant to be read by a person.
//!
//! `--json` for agents, and the container output `logs` and `exec` pass
//! through verbatim. Nothing here is styled, framed or aligned: it is
//! parsed, piped and grepped, and every decoration would be something to
//! strip back off. What a person reads lives in [`crate::ui`].

use kobune_api::{ApiError, Event, LogLevel};

/// What `logs` and `exec` print.
///
/// Undecorated, so it can be grepped through a pipe or read as-is by an
/// agent. The container's stderr stays on stderr.
pub fn print_output_event(event: &Event) {
    match event {
        Event::Output { line, stream, .. } => match stream {
            kobune_api::OutputStream::Stdout => println!("{line}"),
            kobune_api::OutputStream::Stderr => eprintln!("{line}"),
        },
        Event::Log {
            level: LogLevel::Warn,
            message,
        } => eprintln!("warning: {message}"),
        Event::Log {
            level: LogLevel::Error,
            message,
        } => eprintln!("error: {message}"),
        _ => {}
    }
}

/// How errors print under `--json`.
///
/// On stdout, so an agent has nothing to watch but the exit code and one
/// JSON stream.
pub fn print_error_json(error: &ApiError) {
    let payload = serde_json::json!({ "error": error });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
    );
}

pub fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(err) => eprintln!("error: cannot render the response as JSON: {err}"),
    }
}
