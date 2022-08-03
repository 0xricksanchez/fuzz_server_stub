use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, MutexGuard};

type Result<T> = std::result::Result<T, Error>;

/// A dead simple custom Error class that makes error handling less of a pain.
#[derive(Debug)]
pub enum Error {
    SerializeTestcase,

    DroppedConnection,

    NoIndex,

    ReadingPayload,
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
    seen_by: Vec<IpAddr>,
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
async fn get_fuzzpkt_from_payload(stream: &mut TcpStream, addr: &SocketAddr) -> Result<FuzzPacket> {
    let mut buf = vec![0; 512];
    // FIXME: This seems to fail in some cases as it's not pulling all the data from the stream -> causing
    // the data to be all '\0'...
    let _ret = stream.read(&mut buf).await.map_err(|_| Error::ReadingPayload)?;
    if buf.iter().all(|b| *b == 0) || !buf.starts_with(&[42]) {
        return Err(Error::DroppedConnection);
    }

    let tc = FuzzPacket::from(&buf);

    let dstr = String::from_utf8_lossy(&tc.data);
    println!("> Peer: {} | Ver: {} | data length: {} | Data: {:?}", addr, tc.version, tc.length, dstr);
    buf.clear();
    Ok(tc)
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
        // Grab a `FuzzPacket` if possible...
        if let Ok(fuzz_pkt) = get_fuzzpkt_from_payload(socket, addr).await {
            let con_lock = connections.lock().await;
            let con_len = con_lock.clients.len();
            drop(con_lock);
            // If we have at least another client add it to the message_queue so it can be forwarded
            if con_len > 1 {
                let mut msg_lock = message_queue.lock().await;
                msg_lock.messages.push(Message {
                    sender: *addr,
                    content: fuzz_pkt,
                    seen_by: Vec::new(),
                });
                drop(msg_lock);
            }
        } else {
            // If an error occurs during client handling we just drop them
            let _ret = handle_client_drop(addr, &connections).await;
            break;
        }

        // Attempt to consume the message queue if not empty
        let mut msg_lock = message_queue.lock().await;
        if !msg_lock.messages.is_empty() {
            let mut idx_vec = handle_msg_fwd(socket, addr, &connections, &mut msg_lock).await?;
            rm_seen_msgs(&mut msg_lock, &mut idx_vec);
            drop(msg_lock);
        } else {
            continue;
        }
    }
    Ok(())
}

fn rm_seen_msgs(msg_lock: &mut MutexGuard<MessageQueue>, idx_vec: &mut Vec<usize>) {
    idx_vec.reverse();
    for e in idx_vec {
        msg_lock.messages.remove(*e);
    }
}

async fn handle_msg_fwd(socket: &mut TcpStream, addr: &SocketAddr, connections: &Arc<Mutex<Connections>>, msg_lock: &mut MutexGuard<'_, MessageQueue>) -> Result<Vec<usize>> {
    let mut idx_vec: Vec<usize> = Vec::new();
    for (idx, message) in msg_lock.messages.iter_mut().enumerate() {
        if message.sender != *addr && !message.seen_by.contains(&addr.ip()) {
            let serialized_pkt: Vec<u8> = (&message.content).into();
            let _ = socket.write_all(&serialized_pkt).await;
            message.seen_by.push(addr.ip());
            let con_lock = connections.lock().await;
            let con_ipaddr = con_lock.clients.iter().map(|x| x.ip()).collect::<Vec<IpAddr>>();
            if message.seen_by.iter().all(|item| con_ipaddr.contains(item)) {
                idx_vec.push(idx);
            }
        }
    }
    Ok(idx_vec)
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
