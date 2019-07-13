use std::{io::BufReader, thread, process::{Command, Stdio}};

use crossbeam::channel::{bounded, Receiver, Sender};

use lsp_types::notification::Exit;

use super::{msg::RawMessage, Result};

pub fn piped_process_transport(
    command: &str,
    args: Vec<String>,
) -> Result<(Receiver<RawMessage>, Sender<RawMessage>, Threads)> {
    let child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;

    let (writer_sender, writer_receiver) = bounded::<RawMessage>(16);

    let child_stdin = child.stdin.unwrap();
    let child_stdout = child.stdout.unwrap();

    let writer = thread::spawn(move || {
        writer_receiver
            .into_iter()
            .try_for_each(|it| it.write(&mut child_stdin))?;
        Ok(())
    });
    let (reader_sender, reader_receiver) = bounded::<RawMessage>(16);
    let reader = thread::spawn(move || {
        let buf_read = BufReader::new(child_stdout);
        while let Some(msg) = RawMessage::read(&mut buf_read)? {
            let is_exit = match &msg {
                RawMessage::Notification(n) => n.is::<Exit>(),
                _ => false,
            };

            reader_sender.send(msg).unwrap();

            if is_exit {
                break;
            }
        }
        Ok(())
    });
    let threads = Threads { reader, writer };
    Ok((reader_receiver, writer_sender, threads))
}

pub struct Threads {
    reader: thread::JoinHandle<Result<()>>,
    writer: thread::JoinHandle<Result<()>>,
}

impl Threads {
    pub fn join(self) -> Result<()> {
        match self.reader.join() {
            Ok(r) => r?,
            Err(_) => Err("reader panicked")?,
        }
        match self.writer.join() {
            Ok(r) => r,
            Err(_) => Err("writer panicked")?,
        }
    }
}
