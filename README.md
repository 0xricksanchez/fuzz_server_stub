## Readme

Quick PoC of a fuzzing server stub written in Rust.
The server opens a TCP connection on `localhost:5555` and asynchronously handles incoming connections.
The incoming packets are expected to be in a sane form of:

```
[protocol_version:u8][data_length:u16][data:<data_length>]
```

The server only does some basic (de-)serialization of the packet in addition to re-routing packets from one client to another.
A connected client *A* will never get back a message it has sent. 


