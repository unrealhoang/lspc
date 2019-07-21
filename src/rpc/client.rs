use log;
use std::{
    fmt::Display,
    io::{BufRead, BufReader, Read, Write},
    thread,
    time::Duration,
};

use crossbeam::channel::{bounded, Receiver, Sender};

pub trait Message: Sized + Send + 'static {
    type Id: Copy + PartialEq + Send + Display + 'static;

    fn read(r: &mut impl BufRead) -> Result<Option<Self>, RpcError>;
    fn write(self, w: &mut impl Write) -> Result<(), RpcError>;
    fn is_exit(&self) -> bool;
    fn is_response(&self) -> bool;
    fn id(&self) -> Option<Self::Id>;
}

#[derive(Debug)]
pub enum RpcError {
    Deserialize(String),
    Read(String),
    Write(String),
    Serialize(String),
    Timeout,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Deserialize(e) => write!(f, "Deserialize Error: {}", e),
            RpcError::Serialize(e) => write!(f, "Serialize Error: {}", e),
            RpcError::Write(e) => write!(f, "Write Error: {}", e),
            RpcError::Read(e) => write!(f, "Read Error: {}", e),
            Timeout => write!(f, "Request timed out"),
        }
    }
}

impl std::convert::From<RpcError> for String {
    fn from(c: RpcError) -> Self {
        match c {
            RpcError::Deserialize(e) => format!("Deserialize Error: {}", e),
            RpcError::Serialize(e) => format!("Serialize Error: {}", e),
            RpcError::Write(e) => format!("Write Error: {}", e),
            RpcError::Read(e) => format!("Read Error: {}", e),
            Timeout => format!("Request timed out"),
        }
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug)]
pub struct Threads {
    reader: thread::JoinHandle<Result<(), RpcError>>,
    writer: thread::JoinHandle<Result<(), RpcError>>,
}

impl Threads {
    pub fn join(self) -> Result<(), String> {
        match self.reader.join() {
            Ok(r) => r?,
            Err(_) => Err("reader panicked")?,
        };
        match self.writer.join() {
            Ok(r) => r?,
            Err(_) => Err("writer panicked")?,
        };
        Ok(())
    }
}

#[derive(Debug)]
pub struct Client<M>
where
    M: Message,
{
    pub sender: Sender<M>,
    pub receiver: Receiver<M>,
    pub subscription_sender: Sender<(M::Id, Sender<M>)>,
    threads: Threads,
}

impl<M: Message> Client<M> {
    pub fn new<RF, WF, R, W>(get_reader: RF, get_writer: WF) -> Self
    where
        RF: FnOnce() -> R,
        WF: FnOnce() -> W,
        R: Read + Sized,
        W: Write + Sized,
        RF: Send + 'static,
        WF: Send + 'static,
    {
        let (writer_sender, writer_receiver) = bounded::<M>(16);
        let writer = thread::spawn(move || {
            let mut io_writer = get_writer();
            writer_receiver.into_iter().for_each(|msg| {
                if let Err(e) = msg.write(&mut io_writer) {
                    log::error!("Failed to write message {}", e);
                }
            });
            Ok(())
        });

        let (reader_sender, reader_receiver) = bounded::<M>(16);
        let (subscription_sender, subscription_receiver) = bounded::<(M::Id, Sender<M>)>(16);
        let reader = thread::spawn(move || {
            let mut subscriptions = Vec::new();

            let io_reader = get_reader();
            let mut buf_read = BufReader::new(io_reader);
            while let Some(msg) = M::read(&mut buf_read)? {
                let is_exit = msg.is_exit();

                if msg.is_response() {
                    while let Ok(sub) = subscription_receiver.try_recv() {
                        subscriptions.push(sub);
                    }
                    if let Some(index) = subscriptions
                        .iter()
                        .position(|item| item.0 == msg.id().unwrap())
                    {
                        let sub = subscriptions.swap_remove(index);
                        sub.1.send(msg);
                    } else {
                        log::warn!("Received non-requested response: {}", msg.id().unwrap());
                    }
                } else {
                    reader_sender.send(msg).unwrap();
                }

                if is_exit {
                    break;
                }
            }
            Ok(())
        });
        let threads = Threads { reader, writer };

        Client {
            sender: writer_sender,
            receiver: reader_receiver,
            threads,
            subscription_sender,
        }
    }

    pub fn request(&self, req: M) -> Result<M, RpcError> {
        let (response_sender, response_receiver) = bounded::<M>(1);
        self.subscription_sender
            .send((req.id().unwrap(), response_sender))
            .unwrap();
        self.sender.send(req).unwrap();

        response_receiver
            .recv_timeout(Duration::from_secs(60))
            .map_err(|_| RpcError::Timeout)
    }

    fn close(self) -> Result<(), String> {
        self.threads.join()
    }
}
