use crate::{Error, Result};
#[cfg(any(unix, windows))]
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(any(unix, windows))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(unix, windows))]
use std::time::Instant;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(any(unix, windows))]
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
#[cfg(any(unix, windows))]
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// One-shot IPC requests bound to an observed server generation.
/// Generation checks are not a transactional server-side precondition.
pub struct Client {
    socket: PathBuf,
    generation: String,
    timeout: Duration,
}

impl Client {
    pub fn new(socket: PathBuf, generation: String) -> Self {
        Self::with_timeout(socket, generation, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(socket: PathBuf, generation: String, timeout: Duration) -> Self {
        Self {
            socket,
            generation,
            timeout,
        }
    }

    /// Return the result from one validated response on a fresh connection.
    /// A timeout, interruption or disconnect does not prove a mutation was not applied.
    #[cfg(any(unix, windows))]
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        crate::check_interrupted()?;
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or(Error::Invalid("invalid transport timeout"))?;
        let id = format!(
            "last-pane-guard:{}:{}",
            std::process::id(),
            NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
        );
        let mut request = serde_json::to_vec(&serde_json::json!({
            "id": id, "method": method, "params": params,
        }))
        .map_err(|_| Error::Invalid("cannot encode API request"))?;
        request.push(b'\n');
        self.check_generation()?;
        #[cfg(unix)]
        let mut stream = unix::connect(&self.socket, deadline)?;
        #[cfg(windows)]
        let mut stream = pipe::connect(&self.socket, &self.generation, deadline)?;
        // Revalidate after connect and before sending even the first request byte.
        self.check_generation()?;
        platform::write_request(&mut stream, &request, deadline)?;
        let response = platform::read_response(&mut stream, deadline, MAX_RESPONSE_BYTES)?;
        let result = decode_response(&response, &id);
        remaining(deadline)?;
        result
    }

    #[cfg(not(any(unix, windows)))]
    pub fn call(&self, _method: &str, _params: Value) -> Result<Value> {
        let _ = (&self.socket, &self.generation, self.timeout);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "local IPC is not implemented on this platform",
        )
        .into())
    }

    #[cfg(any(unix, windows))]
    fn check_generation(&self) -> Result<()> {
        if crate::state::generation(&self.socket)? != self.generation {
            return Err(Error::Stale);
        }
        Ok(())
    }
}

#[cfg(any(unix, windows))]
#[derive(Deserialize)]
struct Response {
    id: String,
    #[serde(default, deserialize_with = "present_result")]
    result: Option<Value>,
    #[serde(default, deserialize_with = "present_error")]
    error: Option<ApiError>,
}

#[cfg(any(unix, windows))]
#[derive(Deserialize)]
struct ApiError {
    code: String,
}

#[cfg(any(unix, windows))]
fn present_result<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[cfg(any(unix, windows))]
fn present_error<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<ApiError>, D::Error> {
    struct ObjectError;
    impl<'de> serde::de::Visitor<'de> for ObjectError {
        type Value = Option<ApiError>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an API error object")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            ApiError::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Some)
        }
    }
    deserializer.deserialize_map(ObjectError)
}

#[cfg(windows)]
use pipe as platform;
#[cfg(unix)]
use unix as platform;

#[cfg(any(unix, windows))]
fn remaining(deadline: Instant) -> Result<Duration> {
    crate::check_interrupted()?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(Error::Timeout)
    } else {
        Ok(remaining)
    }
}

/// Herdr names a Windows pipe by prefixing the lossy endpoint marker path.
#[cfg_attr(not(windows), allow(dead_code))]
fn pipe_name(socket: &std::path::Path) -> String {
    format!(r"\\.\pipe\{}", socket.to_string_lossy())
}

#[cfg(any(unix, windows))]
fn decode_response(bytes: &[u8], id: &str) -> Result<Value> {
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(Error::Invalid("API response must be an object"));
    }
    let response: Response =
        serde_json::from_slice(bytes).map_err(|_| Error::Invalid("malformed API response"))?;
    if response.id != id {
        return Err(Error::Invalid("API response ID mismatch"));
    }
    match (response.result, response.error) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) if error.code == "server_not_running" => Err(Error::Stopped),
        (None, Some(error)) if !error.code.is_empty() => Err(Error::Api(error.code)),
        _ => Err(Error::Invalid("invalid API response envelope")),
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{self, Read};
    use std::mem::offset_of;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixStream;
    use std::path::Path;

    fn ready(stream: &UnixStream, events: libc::c_short, deadline: Instant) -> Result<()> {
        loop {
            let duration = remaining(deadline)?;
            // Round up for poll, and limit slices to notice cancellation even when
            // a signal arrives between checking the flag and entering poll.
            let millis = duration.as_nanos().div_ceil(1_000_000).min(100);
            let timeout =
                i32::try_from(millis).map_err(|_| Error::Invalid("invalid poll timeout"))?;
            let mut descriptor = libc::pollfd {
                fd: stream.as_raw_fd(),
                events,
                revents: 0,
            };
            // SAFETY: descriptor is initialized and live for the entire poll call.
            let count = unsafe { libc::poll(&mut descriptor, 1, timeout) };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            remaining(deadline)?;
            if count == 0 {
                continue;
            }
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF).into());
            }
            // HUP/ERR also wake the caller, which then gets EOF or the precise I/O error.
            if descriptor.revents & (events | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(());
            }
        }
    }

    pub(super) fn connect(socket: &Path, deadline: Instant) -> Result<UnixStream> {
        let path_bytes = socket.as_os_str().as_bytes();
        // SAFETY: an all-zero sockaddr_un is valid storage before filling its fields.
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        if path_bytes.is_empty()
            || path_bytes.contains(&0)
            || path_bytes.len() >= address.sun_path.len()
        {
            return Err(Error::Invalid("invalid Unix socket path"));
        }
        address.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX)
            .map_err(|_| Error::Invalid("invalid Unix address family"))?;
        for (target, byte) in address.sun_path.iter_mut().zip(path_bytes) {
            *target = libc::c_char::from_ne_bytes([*byte]);
        }
        let address_len = offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1;
        let socket_len = libc::socklen_t::try_from(address_len)
            .map_err(|_| Error::Invalid("invalid Unix socket address length"))?;
        #[cfg(any(
            target_vendor = "apple",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            address.sun_len = u8::try_from(address_len)
                .map_err(|_| Error::Invalid("invalid Unix socket address length"))?;
        }
        loop {
            remaining(deadline)?;
            // SAFETY: socket has no pointer arguments and returns a new owned descriptor.
            let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
            if raw < 0 {
                return Err(io::Error::last_os_error().into());
            }
            // SAFETY: raw was just created and is now owned exactly once.
            let owned = unsafe { OwnedFd::from_raw_fd(raw) };
            let stream = UnixStream::from(owned);
            // SAFETY: the descriptor is live; F_SETFD takes the integer flags argument.
            if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error().into());
            }
            stream.set_nonblocking(true)?;
            remaining(deadline)?;
            // SAFETY: address is initialized, with a bounded length including its NUL.
            let connected = unsafe {
                libc::connect(
                    raw,
                    (&address as *const libc::sockaddr_un).cast(),
                    socket_len,
                )
            };
            if connected == 0 {
                return Ok(stream);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINPROGRESS)
                || error.kind() == io::ErrorKind::Interrupted
            {
                ready(&stream, libc::POLLOUT, deadline)?;
                if let Some(error) = stream.take_error()? {
                    return Err(connect_error(socket, error));
                }
                stream.peer_addr()?;
                return Ok(stream);
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                // A full Unix listener backlog can reject a nonblocking attempt with
                // EAGAIN rather than leave a pending connect. Retry with a fresh socket,
                // never replaying a request after any bytes have been sent.
                drop(stream);
                std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(1)));
                continue;
            }
            return Err(connect_error(socket, error));
        }
    }

    fn connect_error(socket: &Path, error: io::Error) -> Error {
        match std::fs::metadata(socket) {
            Err(missing) if missing.kind() == io::ErrorKind::NotFound => Error::Stopped,
            _ => Error::Io(error),
        }
    }

    pub(super) fn write_request(
        stream: &mut UnixStream,
        mut request: &[u8],
        deadline: Instant,
    ) -> Result<()> {
        while !request.is_empty() {
            remaining(deadline)?;
            // SAFETY: request is valid for its length and the descriptor is live.
            // MSG_NOSIGNAL protects even callers that restore the default SIGPIPE action.
            let count = unsafe {
                libc::send(
                    stream.as_raw_fd(),
                    request.as_ptr().cast(),
                    request.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            let sent = if count < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(usize::try_from(count)
                    .map_err(|_| Error::Invalid("invalid socket write count"))?)
            };
            match sent {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(count) => request = &request[count..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    ready(stream, libc::POLLOUT, deadline)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(super) fn read_response(
        stream: &mut UnixStream,
        deadline: Instant,
        limit: usize,
    ) -> Result<Vec<u8>> {
        let mut response = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            remaining(deadline)?;
            match stream.read(&mut buffer) {
                Ok(0) => return Err(Error::Invalid("API response ended before newline")),
                Ok(count) => {
                    let newline = buffer[..count].iter().position(|byte| *byte == b'\n');
                    let end = newline.unwrap_or(count);
                    if response.len() + end > limit {
                        return Err(Error::Invalid("API response exceeds size limit"));
                    }
                    response.extend_from_slice(&buffer[..end]);
                    if newline.is_some() {
                        return Ok(response);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    ready(stream, libc::POLLIN, deadline)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

/// Windows named-pipe client. Herdr exposes the API as a pipe named after the
/// endpoint marker path, and the marker records the owning server process.
#[cfg(windows)]
mod pipe {
    use super::*;
    use std::ffi::c_void;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::path::Path;

    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    // Never allow the server to impersonate this client beyond identification.
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    const ERROR_BROKEN_PIPE: i32 = 109;
    const ERROR_PIPE_BUSY: i32 = 231;
    const ERROR_MORE_DATA: i32 = 234;
    const ERROR_IO_PENDING: i32 = 997;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;

    // Fields other than the event are owned by the kernel during an operation.
    #[allow(dead_code)]
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: RawHandle,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateEventW(
            attributes: *const c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> RawHandle;
        fn ReadFile(
            file: RawHandle,
            buffer: *mut u8,
            length: u32,
            transferred: *mut u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn WriteFile(
            file: RawHandle,
            buffer: *const u8,
            length: u32,
            transferred: *mut u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn GetOverlappedResult(
            file: RawHandle,
            overlapped: *mut Overlapped,
            transferred: *mut u32,
            wait: i32,
        ) -> i32;
        fn CancelIoEx(file: RawHandle, overlapped: *mut Overlapped) -> i32;
        fn WaitForSingleObject(handle: RawHandle, milliseconds: u32) -> u32;
        fn GetNamedPipeServerProcessId(pipe: RawHandle, process_id: *mut u32) -> i32;
    }

    pub(super) fn connect(socket: &Path, generation: &str, deadline: Instant) -> Result<File> {
        let expected = crate::state::marker_pid(generation)
            .ok_or(Error::Invalid("invalid Herdr endpoint marker"))?;
        let name = pipe_name(socket);
        loop {
            remaining(deadline)?;
            let opened = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(FILE_FLAG_OVERLAPPED)
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(&name);
            match opened {
                Ok(file) => {
                    let mut server = 0_u32;
                    // SAFETY: the handle is live and server is writable storage for one u32.
                    if unsafe { GetNamedPipeServerProcessId(file.as_raw_handle(), &mut server) }
                        == 0
                    {
                        return Err(io::Error::last_os_error().into());
                    }
                    // A pipe served by another process is not the recorded Herdr server.
                    if server != expected {
                        return Err(Error::Stale);
                    }
                    return Ok(file);
                }
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    // All instances are busy; nothing was sent, so a new attempt is safe.
                    std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(10)));
                }
                Err(error) => {
                    return Err(match std::fs::metadata(socket) {
                        Err(missing) if missing.kind() == io::ErrorKind::NotFound => Error::Stopped,
                        _ => Error::Io(error),
                    })
                }
            }
        }
    }

    enum Buffer<'a> {
        Read(&'a mut [u8]),
        Write(&'a [u8]),
    }

    /// Run one overlapped transfer. Kernel use of the buffer and OVERLAPPED storage
    /// always ends before return; cancellation waits for the aborted operation.
    fn transfer(file: &File, buffer: Buffer<'_>, deadline: Instant) -> Result<usize> {
        let handle = file.as_raw_handle();
        // SAFETY: null attributes and name request an unnamed manual-reset event.
        let raw = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: raw is a new event handle and is owned exactly once.
        let event = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: event.as_raw_handle(),
        };
        let reading = matches!(buffer, Buffer::Read(_));
        let started = match buffer {
            Buffer::Read(bytes) => {
                let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                // SAFETY: bytes and overlapped outlive the operation, see completion below.
                unsafe {
                    ReadFile(
                        handle,
                        bytes.as_mut_ptr(),
                        length,
                        std::ptr::null_mut(),
                        &mut overlapped,
                    )
                }
            }
            Buffer::Write(bytes) => {
                let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                // SAFETY: bytes and overlapped outlive the operation, see completion below.
                unsafe {
                    WriteFile(
                        handle,
                        bytes.as_ptr(),
                        length,
                        std::ptr::null_mut(),
                        &mut overlapped,
                    )
                }
            }
        };
        if started == 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(ERROR_IO_PENDING) => wait(handle, &event, &mut overlapped, deadline)?,
                Some(ERROR_MORE_DATA) => (),
                Some(ERROR_BROKEN_PIPE) if reading => return Ok(0),
                _ => return Err(error.into()),
            }
        }
        let mut transferred = 0_u32;
        // SAFETY: the operation on this handle and storage has completed.
        if unsafe { GetOverlappedResult(handle, &mut overlapped, &mut transferred, 0) } == 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(ERROR_MORE_DATA) => (),
                Some(ERROR_BROKEN_PIPE) if reading => return Ok(0),
                _ => return Err(error.into()),
            }
        }
        usize::try_from(transferred).map_err(|_| Error::Invalid("invalid pipe transfer count"))
    }

    fn wait(
        handle: RawHandle,
        event: &OwnedHandle,
        overlapped: &mut Overlapped,
        deadline: Instant,
    ) -> Result<()> {
        loop {
            let left = match remaining(deadline) {
                Ok(left) => left,
                Err(error) => {
                    cancel(handle, overlapped);
                    return Err(error);
                }
            };
            // Short slices keep deadline and interruption checks responsive.
            let millis = u32::try_from(left.as_millis().clamp(1, 100)).unwrap_or(100);
            // SAFETY: event is a live handle owned by the caller.
            match unsafe { WaitForSingleObject(event.as_raw_handle(), millis) } {
                WAIT_OBJECT_0 => return Ok(()),
                WAIT_TIMEOUT => (),
                _ => {
                    let error = io::Error::last_os_error();
                    cancel(handle, overlapped);
                    return Err(error.into());
                }
            }
        }
    }

    /// Cancel one pending operation and wait until the kernel releases its storage.
    /// This is not hard real-time: completion of the cancellation is awaited.
    fn cancel(handle: RawHandle, overlapped: &mut Overlapped) {
        let mut transferred = 0_u32;
        // SAFETY: targets only this pending operation; a completed one is unaffected.
        unsafe { CancelIoEx(handle, overlapped) };
        // SAFETY: blocks until this operation completes, so storage stays valid.
        unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, 1) };
    }

    pub(super) fn write_request(
        file: &mut File,
        mut request: &[u8],
        deadline: Instant,
    ) -> Result<()> {
        while !request.is_empty() {
            match transfer(file, Buffer::Write(request), deadline)? {
                0 => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                count => request = &request[count..],
            }
        }
        Ok(())
    }

    pub(super) fn read_response(
        file: &mut File,
        deadline: Instant,
        limit: usize,
    ) -> Result<Vec<u8>> {
        let mut response = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let count = transfer(file, Buffer::Read(&mut buffer), deadline)?;
            if count == 0 {
                return Err(Error::Invalid("API response ended before newline"));
            }
            let newline = buffer[..count].iter().position(|byte| *byte == b'\n');
            let end = newline.unwrap_or(count);
            if response.len() + end > limit {
                return Err(Error::Invalid("API response exceeds size limit"));
            }
            response.extend_from_slice(&buffer[..end]);
            if newline.is_some() {
                return Ok(response);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::thread;
    use tempfile::TempDir;

    fn endpoint() -> (TempDir, PathBuf, UnixListener, String) {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let generation = crate::state::generation(&socket).unwrap();
        (directory, socket, listener, generation)
    }

    fn accept(listener: &UnixListener) -> UnixStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    return stream;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "test server accept timed out");
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("test server accept failed: {error}"),
            }
        }
    }

    fn request(stream: &UnixStream) -> Value {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        assert!(line.ends_with('\n'));
        serde_json::from_str(&line).unwrap()
    }

    fn exchange(response: impl FnOnce(Value) -> Vec<u8> + Send + 'static) -> Result<Value> {
        let (_directory, socket, listener, generation) = endpoint();
        let worker = thread::spawn(move || {
            let mut stream = accept(&listener);
            let value = request(&stream);
            stream.write_all(&response(value)).unwrap();
        });
        let result = Client::new(socket, generation).call("workspace.list", json!({}));
        worker.join().unwrap();
        result
    }

    #[test]
    fn fragmented_response_and_request_encoding() {
        let (_directory, socket, listener, generation) = endpoint();
        let worker = thread::spawn(move || {
            let mut stream = accept(&listener);
            let value = request(&stream);
            assert_eq!(value["method"], "workspace.create");
            assert_eq!(value["params"]["label"], "line\n\"quoted\"");
            let response = format!("{}\n", json!({"id": value["id"], "result": {"ok": true}}));
            for chunk in response.as_bytes().chunks(3) {
                stream.write_all(chunk).unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        });
        let result = Client::new(socket, generation)
            .call("workspace.create", json!({"label": "line\n\"quoted\""}))
            .unwrap();
        assert_eq!(result, json!({"ok": true}));
        worker.join().unwrap();
    }

    #[test]
    fn windows_pipe_name_prefixes_lossy_marker_path() {
        let marker = std::path::Path::new(r"C:\Users\a b\é\herdr.sock");
        assert_eq!(pipe_name(marker), r"\\.\pipe\C:\Users\a b\é\herdr.sock");
    }

    #[test]
    fn returns_null_result() {
        let result = exchange(|request| {
            format!("{}\n", json!({"id": request["id"], "result": null})).into_bytes()
        });
        assert_eq!(result.unwrap(), Value::Null);
    }

    #[test]
    fn validates_response_id() {
        for id in [json!("wrong"), json!(7), Value::Null] {
            let result =
                exchange(move |_| format!("{}\n", json!({"id": id, "result": {}})).into_bytes());
            assert!(matches!(result, Err(Error::Invalid(_))));
        }
    }

    #[test]
    fn preserves_api_error_codes() {
        for code in ["pane_not_found", "permission_denied", "unknown_method"] {
            let result = exchange(move |request| {
                format!(
                    "{}\n",
                    json!({"id": request["id"], "error": {"code": code}})
                )
                .into_bytes()
            });
            assert!(matches!(result, Err(Error::Api(value)) if value == code));
        }
        let result = exchange(|request| {
            format!(
                "{}\n",
                json!({"id": request["id"], "error": {"code": "server_not_running"}})
            )
            .into_bytes()
        });
        assert!(matches!(result, Err(Error::Stopped)));
    }

    #[test]
    fn rejects_malformed_and_conflicting_envelopes() {
        for body in [
            "not-json",
            "[]",
            r#"["$ID",{}]"#,
            "{}",
            r#"{"id":"$ID"}"#,
            r#"{"id":"$ID","result":{},"error":{"code":"server_not_running"}}"#,
            r#"{"id":"$ID","result":{},"error":null}"#,
            r#"{"id":"$ID","error":{}}"#,
            r#"{"id":"$ID","error":["server_not_running"]}"#,
            r#"{"id":"$ID","error":{"code":""}}"#,
            r#"{"id":"$ID","error":{"code":"pane_not_found","code":"server_not_running"}}"#,
            r#"{"id":"$ID","result":{},"result":null}"#,
            r#"{"id":"wrong","id":"$ID","result":{}}"#,
        ] {
            let result = exchange(move |request| {
                format!("{}\n", body.replace("$ID", request["id"].as_str().unwrap())).into_bytes()
            });
            assert!(matches!(result, Err(Error::Invalid(_))), "{body}");
        }
    }

    #[test]
    fn rejects_eof_before_newline() {
        let result = exchange(|request| {
            json!({"id": request["id"], "result": {}})
                .to_string()
                .into_bytes()
        });
        assert!(matches!(result, Err(Error::Invalid(_))));
    }

    #[test]
    fn stopped_and_refused_are_distinct() {
        let (directory, socket, listener, generation) = endpoint();
        drop(listener);
        assert!(socket.exists());
        let result = Client::new(socket, generation).call("pane.get", json!({}));
        assert!(matches!(result, Err(Error::Io(_))));
        let result =
            Client::new(directory.path().join("missing"), "0".into()).call("pane.get", json!({}));
        assert!(matches!(result, Err(Error::Stopped)));
    }

    #[test]
    fn stale_generation_sends_nothing() {
        let (_directory, socket, listener, _generation) = endpoint();
        listener.set_nonblocking(true).unwrap();
        let result = Client::new(socket, "stale".into()).call("workspace.create", json!({}));
        assert!(matches!(result, Err(Error::Stale)));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn checks_generation_on_every_request() {
        let (_directory, socket, listener, generation) = endpoint();
        let moved = socket.with_extension("old");
        let replace_path = socket.clone();
        let worker = thread::spawn(move || {
            let mut stream = accept(&listener);
            let value = request(&stream);
            std::fs::rename(&replace_path, moved).unwrap();
            let replacement = UnixListener::bind(&replace_path).unwrap();
            stream
                .write_all(format!("{}\n", json!({"id": value["id"], "result": {}})).as_bytes())
                .unwrap();
            replacement
        });
        let client = Client::new(socket, generation);
        client.call("pane.list", json!({})).unwrap();
        let replacement = worker.join().unwrap();
        replacement.set_nonblocking(true).unwrap();
        assert!(matches!(
            client.call("workspace.create", json!({})),
            Err(Error::Stale)
        ));
        assert_eq!(
            replacement.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn full_listener_backlog_is_bounded() {
        use std::os::fd::AsRawFd;
        let (_directory, socket, listener, generation) = endpoint();
        // SAFETY: the listener descriptor is live; this only limits its accept backlog.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let mut queued = Vec::new();
        let mut saturated = false;
        for _ in 0..256 {
            match unix::connect(&socket, Instant::now() + Duration::from_millis(20)) {
                Ok(stream) => queued.push(stream),
                Err(Error::Timeout) => {
                    saturated = true;
                    break;
                }
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    saturated = true;
                    break;
                }
                Err(error) => panic!("unexpected backlog error: {error}"),
            }
        }
        assert!(saturated, "test listener backlog was not filled");
        let start = Instant::now();
        let result = Client::with_timeout(socket, generation, Duration::from_millis(40))
            .call("pane.list", json!({}));
        assert!(
            matches!(result, Err(Error::Timeout))
                || matches!(result, Err(Error::Io(ref error)) if error.kind() == std::io::ErrorKind::ConnectionRefused)
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn times_out_without_response() {
        let (_directory, socket, listener, generation) = endpoint();
        let worker = thread::spawn(move || {
            let stream = accept(&listener);
            request(&stream);
            thread::sleep(Duration::from_millis(150));
        });
        let start = Instant::now();
        let result = Client::with_timeout(socket, generation, Duration::from_millis(50))
            .call("pane.list", json!({}));
        assert!(matches!(result, Err(Error::Timeout)));
        assert!(start.elapsed() < Duration::from_secs(1));
        worker.join().unwrap();
    }

    #[test]
    fn slow_drip_does_not_reset_deadline() {
        let (_directory, socket, listener, generation) = endpoint();
        let worker = thread::spawn(move || {
            let mut stream = accept(&listener);
            request(&stream);
            for _ in 0..40 {
                if stream.write_all(b" ").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        let start = Instant::now();
        let result = Client::with_timeout(socket, generation, Duration::from_millis(60))
            .call("pane.list", json!({}));
        assert!(matches!(result, Err(Error::Timeout)));
        assert!(start.elapsed() < Duration::from_millis(350));
        worker.join().unwrap();
    }

    #[test]
    fn blocked_write_uses_same_deadline() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.set_nonblocking(true).unwrap();
        receiver.set_nonblocking(true).unwrap();
        // Prepare data before the deadline so this measures blocked writes, not encoding.
        let request = vec![b'x'; 1024 * 1024];
        let started = Instant::now();
        let deadline = started + Duration::from_millis(150);
        let result = unix::write_request(&mut sender, &request, deadline);
        assert!(matches!(result, Err(Error::Timeout)));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(remaining(deadline), Err(Error::Timeout)));
        // Some bytes must have been sent before the buffer filled and the deadline expired.
        let mut byte = [0];
        assert_eq!(receiver.read(&mut byte).unwrap(), 1);
        assert_eq!(byte[0], b'x');
    }

    #[test]
    fn oversized_response_is_rejected() {
        let (_directory, socket, listener, generation) = endpoint();
        let worker = thread::spawn(move || {
            let mut stream = accept(&listener);
            request(&stream);
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let chunk = [b' '; 8192];
            for _ in 0..=MAX_RESPONSE_BYTES / chunk.len() {
                if stream.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
        let result = Client::new(socket, generation).call("pane.list", json!({}));
        assert!(matches!(
            result,
            Err(Error::Invalid("API response exceeds size limit"))
        ));
        worker.join().unwrap();
    }

    #[test]
    fn response_limit_excludes_newline() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        writer.write_all(b"1234\n").unwrap();
        assert_eq!(
            unix::read_response(&mut reader, Instant::now() + Duration::from_secs(1), 4).unwrap(),
            b"1234"
        );
    }

    #[test]
    fn rejects_overlong_socket_path() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("x".repeat(120));
        std::fs::write(&socket, b"").unwrap();
        let generation = crate::state::generation(&socket).unwrap();
        let result = Client::new(socket, generation).call("pane.list", json!({}));
        assert!(matches!(
            result,
            Err(Error::Invalid("invalid Unix socket path"))
        ));
    }

    #[test]
    fn partial_request_writes_are_completed() {
        let (_directory, socket, listener, generation) = endpoint();
        let worker = thread::spawn(move || {
            let mut stream = accept(&listener);
            thread::sleep(Duration::from_millis(20));
            let value = request(&stream);
            assert_eq!(value["params"]["data"].as_str().unwrap().len(), 1024 * 1024);
            stream
                .write_all(format!("{}\n", json!({"id": value["id"], "result": true})).as_bytes())
                .unwrap();
        });
        let result = Client::new(socket, generation)
            .call("workspace.create", json!({"data": "x".repeat(1024 * 1024)}));
        assert_eq!(result.unwrap(), json!(true));
        worker.join().unwrap();
    }

    #[test]
    fn disconnected_writer_returns_error_without_sigpipe() {
        const CHILD: &str = "LPG_TRANSPORT_SIGPIPE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "transport::tests::disconnected_writer_returns_error_without_sigpipe",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "SIGPIPE subprocess: {output:?}");
            return;
        }
        use std::os::fd::{FromRawFd, OwnedFd};
        let mut descriptors = [-1; 2];
        // SAFETY: this subprocess runs only this test; descriptors has space for two fds.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            assert_eq!(
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_STREAM,
                    0,
                    descriptors.as_mut_ptr()
                ),
                0
            );
        }
        // SAFETY: socketpair created both owned descriptors, with no SO_NOSIGPIPE setup.
        let (mut writer, reader) = unsafe {
            (
                UnixStream::from(OwnedFd::from_raw_fd(descriptors[0])),
                UnixStream::from(OwnedFd::from_raw_fd(descriptors[1])),
            )
        };
        drop(reader);
        assert!(matches!(
            unix::write_request(
                &mut writer,
                b"request\n",
                Instant::now() + Duration::from_secs(1)
            ),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn zero_timeout_does_not_connect() {
        let (_directory, socket, listener, generation) = endpoint();
        listener.set_nonblocking(true).unwrap();
        assert!(matches!(
            Client::with_timeout(socket, generation, Duration::ZERO).call("pane.list", json!({})),
            Err(Error::Timeout)
        ));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn distinct_request_ids_and_connections() {
        let (_directory, socket, listener, generation) = endpoint();
        let worker = thread::spawn(move || {
            let mut ids = Vec::new();
            for _ in 0..2 {
                let mut stream = accept(&listener);
                let value = request(&stream);
                ids.push(value["id"].as_str().unwrap().to_owned());
                stream
                    .write_all(format!("{}\n", json!({"id": value["id"], "result": {}})).as_bytes())
                    .unwrap();
                let mut next = [0; 1];
                assert_eq!(stream.read(&mut next).unwrap(), 0);
            }
            assert_ne!(ids[0], ids[1]);
        });
        let client = Client::new(socket, generation);
        client.call("pane.list", json!({})).unwrap();
        client.call("pane.list", json!({})).unwrap();
        worker.join().unwrap();
    }
}
