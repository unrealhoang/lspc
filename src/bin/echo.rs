use std::io::{Read, Write};
use std::process::Command;
use std::process::Stdio;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target = std::env::args().skip(1).next().expect("command missing");
    let mut child = Command::new(target)
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .expect("Failed to execute command");
    {
        let stdin = child.stdin.as_mut().expect("Failed to open stdin");
        stdin.write_all(b"\x93\x02\xA5hello\x90").unwrap();
    }
    {
        let stdout = child.stdout.as_mut().expect("Failed to open stdout");
        let mut buf = Vec::new();
        loop {
            stdout.read(&mut buf).unwrap();
            if buf.len() > 0 {
                eprintln!("ECHO Received: {:?}", buf);
            }
        }
    }

    Ok(())
}
