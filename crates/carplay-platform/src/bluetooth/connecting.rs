//! Own the pending connection through completion, timeout and cancellation.
use socket2::{SockAddr, Socket};
use std::{
    io,
    net::Shutdown,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const CONNECT_POLL: Duration = Duration::from_millis(50);

struct PendingSocket(Option<Socket>);

impl PendingSocket {
    fn socket(&self) -> &Socket {
        self.0.as_ref().expect("pending socket owns its handle")
    }

    fn connected(mut self, cancelled: &AtomicBool) -> io::Result<Socket> {
        check_cancelled(cancelled)?;
        self.socket().set_nonblocking(false)?;
        Ok(self.0.take().expect("pending socket owns its handle"))
    }
}

impl Drop for PendingSocket {
    fn drop(&mut self) {
        if let Some(socket) = &self.0 {
            // A timed-out nonblocking connect can still be in progress. Release
            // its channel before returning to the caller's reconnect loop, not
            // only when a later process exit tears down the Winsock provider.
            // Bluetooth shutdown disconnects the radio; it has no TCP half-close.
            // https://learn.microsoft.com/windows/win32/bluetooth/bluetooth-and-shutdown
            let _ = socket.shutdown(Shutdown::Both);
            // Abort failed attempts rather than retaining an unsent connection
            // in the provider's graceful-close queue. Some Bluetooth providers
            // do not implement linger; explicit shutdown/handle close still run.
            let _ = socket.set_linger(Some(Duration::ZERO));
        }
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "RFCOMM connection cancelled",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn connect(
    socket: Socket,
    remote: &SockAddr,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> io::Result<Socket> {
    let pending = PendingSocket(Some(socket));
    check_cancelled(cancelled)?;
    pending.socket().set_nonblocking(true)?;
    match pending.socket().connect(remote) {
        Ok(()) => pending.connected(cancelled),
        Err(error) if in_progress(&error) => {
            wait_for_connection(pending, timeout, cancelled, poll_connected)
        }
        Err(error) => Err(error),
    }
}

fn in_progress(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(target_os = "linux")]
    if error.raw_os_error() == Some(libc::EINPROGRESS) {
        return true;
    }
    false
}

fn wait_for_connection(
    pending: PendingSocket,
    timeout: Duration,
    cancelled: &AtomicBool,
    mut poll: impl FnMut(&Socket, Duration) -> io::Result<bool>,
) -> io::Result<Socket> {
    let started = Instant::now();
    loop {
        check_cancelled(cancelled)?;
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "RFCOMM connection timed out",
            ));
        }
        match poll(pending.socket(), remaining.min(CONNECT_POLL)) {
            Ok(true) => return pending.connected(cancelled),
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_os = "windows")]
fn poll_connected(socket: &Socket, timeout: Duration) -> io::Result<bool> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        FD_SET, SOCKET_ERROR, TIMEVAL, WSAGetLastError, select,
    };

    let mut writable = FD_SET {
        fd_count: 1,
        fd_array: [0; 64],
    };
    writable.fd_array[0] = socket.as_raw_socket() as _;
    let mut failed = writable;
    let timeout = TIMEVAL {
        tv_sec: 0,
        tv_usec: timeout.as_micros() as i32,
    };
    // select is supported by the Bluetooth Winsock provider. socket2's generic
    // connect_timeout uses WSAPoll and cannot observe our cancellation token.
    // SAFETY: initialized sets contain one live socket, and remain valid for the
    // bounded call. This thread exclusively owns and closes that socket.
    let ready = unsafe {
        select(
            0,
            std::ptr::null_mut(),
            &mut writable,
            &mut failed,
            &timeout,
        )
    };
    if ready == SOCKET_ERROR {
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    if ready == 0 {
        return Ok(false);
    }
    if let Some(error) = socket.take_error()? {
        return Err(error);
    }
    if failed.fd_count > 0 {
        return Err(io::Error::other(
            "RFCOMM connection failed without a socket error",
        ));
    }
    Ok(writable.fd_count > 0)
}

#[cfg(target_os = "linux")]
fn poll_connected(socket: &Socket, timeout: Duration) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut descriptor = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: the descriptor references this thread's live socket, and poll only
    // writes the initialized stack descriptor during this bounded call.
    let ready = unsafe { libc::poll(&mut descriptor, 1, timeout.as_millis().max(1) as i32) };
    if ready < 0 {
        return Err(io::Error::last_os_error());
    }
    if ready == 0 {
        return Ok(false);
    }
    if let Some(error) = socket.take_error()? {
        return Err(error);
    }
    if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        return Err(io::Error::other(
            "RFCOMM connection failed without a socket error",
        ));
    }
    Ok(descriptor.revents & libc::POLLOUT != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use socket2::{Domain, Type};
    use std::{
        io::Read,
        net::{TcpListener, TcpStream},
        sync::Arc,
        thread,
    };

    fn localhost_pair(listener: &TcpListener) -> (Socket, TcpStream) {
        let socket = connect(
            Socket::new(Domain::IPV4, Type::STREAM, None).unwrap(),
            &listener.local_addr().unwrap().into(),
            Duration::from_secs(2),
            &AtomicBool::new(false),
        )
        .unwrap();
        let (peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        (socket, peer)
    }

    fn assert_peer_closed(mut peer: TcpStream) {
        let result = peer.read(&mut [0]);
        assert!(
            matches!(result, Ok(0))
                || result.is_err_and(|error| matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                )),
            "pending socket was not closed"
        );
    }

    #[test]
    fn pending_timeout_closes_real_socket_before_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (socket, peer) = localhost_pair(&listener);
        let result = wait_for_connection(
            PendingSocket(Some(socket)),
            Duration::from_millis(15),
            &AtomicBool::new(false),
            |_, wait| {
                thread::sleep(wait);
                Ok(false)
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_peer_closed(peer);
        // Retry the exact same real endpoint immediately after timeout.
        let (retry, _) = localhost_pair(&listener);
        drop(retry);
    }

    #[test]
    fn pending_cancel_closes_real_socket_without_waiting_for_connect_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (socket, peer) = localhost_pair(&listener);
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let start = Instant::now();
        let worker = thread::spawn(move || {
            wait_for_connection(
                PendingSocket(Some(socket)),
                Duration::from_secs(15),
                &flag,
                |_, wait| {
                    let _ = entered.try_send(());
                    thread::sleep(wait);
                    Ok(false)
                },
            )
        });
        waiting.recv_timeout(Duration::from_secs(1)).unwrap();
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_peer_closed(peer);
        let (retry, _) = localhost_pair(&listener);
        drop(retry);
    }

    #[test]
    fn failed_completion_and_cancelled_success_close_the_owned_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (socket, peer) = localhost_pair(&listener);
        let result = wait_for_connection(
            PendingSocket(Some(socket)),
            Duration::from_secs(1),
            &AtomicBool::new(false),
            |_, _| Err(io::ErrorKind::ConnectionRefused.into()),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionRefused);
        assert_peer_closed(peer);
        let (socket, peer) = localhost_pair(&listener);
        let cancelled = AtomicBool::new(false);
        let result = wait_for_connection(
            PendingSocket(Some(socket)),
            Duration::from_secs(1),
            &cancelled,
            |_, _| {
                cancelled.store(true, Ordering::Release);
                Ok(true)
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_peer_closed(peer);
    }
}
