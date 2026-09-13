//! Stdout/stderr writing that survives a closed pipe. `println!` panics on EPIPE;
//! a hook whose stdout died (13 such failures in [Q3]) must still exit cleanly.

use std::io::Write;

pub fn out(s: &str) {
    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    let _ = h.write_all(s.as_bytes());
    let _ = h.write_all(b"\n");
    let _ = h.flush();
}

pub fn err(s: &str) {
    let stderr = std::io::stderr();
    let mut h = stderr.lock();
    let _ = h.write_all(s.as_bytes());
    let _ = h.write_all(b"\n");
    let _ = h.flush();
}

pub fn json<T: serde::Serialize>(v: &T) {
    match serde_json::to_string(v) {
        Ok(s) => out(&s),
        Err(e) => err(&format!("muninn: json: {e}")),
    }
}
