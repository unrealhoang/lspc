use std::{
    io::{BufReader, Read, Write, BufRead},
    thread,
};

use crossbeam::channel::{bounded, Receiver, Sender};

use super::Result;

pub trait Message: Sized + Send + 'static {
    fn read(r: &mut impl BufRead) -> Result<Option<Self>>;
    fn write(self, w: &mut impl Write) -> Result<()>;
    fn is_exit(&self) -> bool;
}

#[derive(Debug)]
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

#[derive(Debug)]
pub struct Client<M>
where
    M: Message
{
    pub sender: Sender<M>,
    pub receiver: Receiver<M>,
    threads: Threads,
}

impl<M: Message> Client<M> {
    pub fn new<R, W>(io_reader: R, mut io_writer: W) -> Result<Self>
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let (writer_sender, writer_receiver) = bounded::<M>(16);
        let (reader_sender, reader_receiver) = bounded::<M>(16);

        let writer = thread::spawn(move || {
            writer_receiver
                .into_iter()
                .try_for_each(|msg| msg.write(&mut io_writer))?;
            Ok(())
        });

        let (reader_sender, reader_receiver) = bounded::<M>(16);
        let reader = thread::spawn(move || {
            let mut buf_read = BufReader::new(io_reader);
            while let Some(msg) = M::read(&mut buf_read)? {
                let is_exit = msg.is_exit();

                reader_sender.send(msg).unwrap();

                if is_exit {
                    break;
                }
            }
            Ok(())
        });
        let threads = Threads { reader, writer };

        let client = Client {
            sender: writer_sender,
            receiver: reader_receiver,
            threads,
        };
        Ok(client)
    }

    fn close(mut self) {
        self.threads.join();
    }
}
