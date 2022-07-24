use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

type Result<T> = std::result::Result<T, Error>;

/// A dead simple custom Error class that makes error handling less of a pain.
#[derive(Debug)]
pub enum Error {
    SerializeTestcase,

    DroppedConnection,

    NoIndex,
}

/// Keep track of incoming clients
#[derive(Debug, Default)]
struct Connections {
    clients: Vec<SocketAddr>,
}

/// Track the sender and the contents of a received packet
#[derive(Debug, Clone)]
struct Message {
    sender: SocketAddr,
    content: FuzzPacket,
}

/// Stash every message in here and mimic a queue behavior
#[derive(Debug, Clone, Default)]
struct MessageQueue {
    messages: Vec<Message>,
}

/// Dummy packet to be received
#[repr(C)]
#[derive(Default, Debug, Clone)]
struct FuzzPacket {
    // Some version
    version: u8,
    // Length of the payload
    length: u16,
    // payload
    data: Vec<u8>,
}

/// Deserialize buffer into a FuzzPacket structure
impl From<&Vec<u8>> for FuzzPacket {
    fn from(buf: &Vec<u8>) -> Self {
        let version = buf[0];
        let length = ((buf[1] as u16) << 8) | buf[2] as u16;
        let data = buf[3..=(length + 2) as usize].to_vec();

        FuzzPacket {
            version,
            length,
            data,
        }
    }
}

/// Serialize FuzzPacket back into a Vec<u8>
impl From<&FuzzPacket> for Vec<u8> {
    fn from(pkt: &FuzzPacket) -> Self {
        let mut s = Vec::new();
        s.push(pkt.version);
        let mut tmp = u16::to_be_bytes(pkt.length).to_vec();
        s.append(&mut tmp);
        for b in pkt.data.clone() {
            s.push(b);
        }
        s
    }

}

/// Handle incoming connections
/// Print the deserialized test case
/// Add sender and its message to the message queue
async fn handle_client(stream: &mut TcpStream,  addr: &SocketAddr,  message_queue: &Arc<Mutex<MessageQueue>>) -> Result<()> {
    let mut msg_lock = message_queue.lock().await;
    let mut buf = vec![0; 512];
    let _ret = stream.read(&mut buf).await.expect("Reading into buf");
    if buf.clone().into_iter().all(|b| b == 0) || !buf.starts_with(&[42]) {
        return Err(Error::DroppedConnection);
    }

    let tc = FuzzPacket::from(&buf);
    msg_lock.messages.push(Message {
        sender: *addr,
        content: tc.clone(),
    });
    let dstr = String::from_utf8_lossy(&tc.data);
    println!("> Peer: {addr} | Ver: {} | data length: {} | Data: {:?}",  tc.version, tc.length, dstr);
    buf.clear();
    //let queue_length = msg_lock.messages.len();
    //println!("queue_length: {}", queue_length);
    Ok(())
}

/// Drop client from client list
async fn handle_client_drop(addr: &SocketAddr, connections: &Arc<Mutex<Connections>>) -> Result<()> {
    let mut con_lock = connections.lock().await;
    let index = con_lock
        .clients
        .iter()
        .position(|&r| r == *addr)
        .ok_or(Error::NoIndex)?;
    println!("Dropping client: {addr} @ idx {index}");
    con_lock.clients.remove(index);
    drop(con_lock);
    Ok(())
}

// Keep on reading the socket, handling the requests
async fn worker(socket: &mut TcpStream, addr: &SocketAddr, message_queue: &Arc<Mutex<MessageQueue>>, connections: &Arc<Mutex<Connections>>) -> Result<()> {
    let message_queue = Arc::clone(message_queue);
    let connections = Arc::clone(connections);
    loop {
        // If an error occurs during client handling we just drop them
        if handle_client(socket, addr, &message_queue).await.is_err() {
            let _ret = handle_client_drop(addr, &connections).await;
            break;
        }
        // Attempt to consume the message queue if not empty
        let mut msg_lock = message_queue.lock().await;
        if !msg_lock.messages.is_empty() {
            let mut idx_vec: Vec<usize> = Vec::new();
            for (idx, message) in msg_lock.messages.clone().iter().enumerate() {
                if message.sender != *addr {
                    let serialized_pkt: Vec<u8> = (&message.content).into();
                    //println!("Sending {} -> {}: {:?}", message.sender, addr, serialized_pkt);
                    let _ = socket.write_all(&serialized_pkt).await;
                    idx_vec.push(idx);
                }
            }
            idx_vec.reverse();
            for entry in idx_vec {
                msg_lock.messages.remove(entry);
            }
            drop(msg_lock);
        } else {
            continue
        }
    }
    Ok(())
}


#[tokio::main]
async fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:5555").await?;
    let connections = Arc::new(Mutex::new(Connections::default()));
    let message_queue = Arc::new(Mutex::new(MessageQueue::default()));
    loop {
        let message_queue = Arc::clone(&message_queue);
        let connections = Arc::clone(&connections);
        // Asynchronously wait for an inbound connection
        let (mut socket, addr) = listener.accept().await?;
        // Push new client info into Vec
        let mut con_lock = connections.lock().await;
        if !con_lock.clients.contains(&addr) {
            println!("Adding new client: {addr}");
            con_lock.clients.push(addr);
        }
        drop(con_lock);
        println!("Client-List: {:?}", connections);

        // Spawn asynchronous background worker
        tokio::spawn(async move {
            let _ret = worker(&mut socket, &addr, &message_queue, &connections).await;
        });
    }
}
