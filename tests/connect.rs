use std::{
    io::Read,
    mem::MaybeUninit,
    net::{Ipv4Addr, TcpListener},
    os::windows::io::AsRawSocket,
    time::Duration,
};

use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use wepoll2::{Event, PollMode, Poller};

#[test]
fn poll_connect() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();

    let client = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).unwrap();
    client.set_nonblocking(true).unwrap();

    let mut poller = Poller::new().unwrap();
    let mut interest = Event::none(114514);
    interest.set_writable(true);
    poller
        .add(client.as_raw_socket() as _, interest, PollMode::Level)
        .unwrap();

    let e = client.connect(&SockAddr::from(addr)).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock);

    let (mut server, _) = listener.accept().unwrap();

    let mut buf = vec![0u8; 1 << 20];
    for round in 0..5 {
        let mut entries = [MaybeUninit::uninit(); 8];
        let mut len = poller.wait(&mut entries, None, false).unwrap();

        buf.fill(round);
        let mut bytes_sent = 0;
        loop {
            assert_eq!(len, 1);
            let event = Event::from(unsafe { MaybeUninit::assume_init_ref(&entries[0]) });
            assert_eq!(event.key, 114514);
            assert!(event.is_writable());

            match client.send(&buf) {
                Ok(res) => bytes_sent += res,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{:?}", e),
            }
            len = poller
                .wait(&mut entries, Some(Duration::ZERO), false)
                .unwrap();
            if len == 0 {
                break;
            }
        }

        let mut bytes_received = 0;
        loop {
            let res = server.read(&mut buf).unwrap();
            assert_eq!(buf[0], round);
            bytes_received += res;
            if bytes_received >= bytes_sent {
                break;
            }
        }
        assert_eq!(bytes_received, bytes_sent);
    }

    poller.delete(client.as_raw_socket() as _).unwrap();
}
