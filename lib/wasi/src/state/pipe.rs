use crate::syscalls::types::*;
use crate::syscalls::{read_bytes, write_bytes};
use bytes::{Buf, Bytes};
use std::convert::TryInto;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::io::{Read, Seek, Write};
use std::ops::DerefMut;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Mutex;
use std::time::Duration;
use wasmer::WasmSlice;
use wasmer::{MemorySize, MemoryView};
use wasmer_vfs::{FsError, VirtualFile};
use wasmer_wasi_types::wasi::Errno;
use wasmer_vfs::VirtualFile;

#[derive(Debug)]
pub struct WasiPipe {
    /// Sends bytes down the pipe
    tx: Mutex<mpsc::Sender<Vec<u8>>>,
    /// Receives bytes from the pipe
    rx: Mutex<mpsc::Receiver<Vec<u8>>>,
    /// Buffers the last read message from the pipe while its being consumed
    read_buffer: Mutex<Option<Bytes>>,
    /// Whether the pipe should block or not block to wait for stdin reads
    block: bool,
}

/// Pipe pair of (a, b) WasiPipes that are connected together
#[derive(Debug)]
pub struct WasiBidirectionalPipePair {
    pub send: WasiPipe,
    pub recv: WasiPipe,
}

impl Write for WasiBidirectionalPipePair {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.send.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.send.flush()
    }
}

impl Seek for WasiBidirectionalPipePair {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Ok(0)
    }
}

impl Read for WasiBidirectionalPipePair {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.recv.read(buf)
    }
}

impl VirtualFile for WasiBidirectionalPipePair {
    fn last_accessed(&self) -> u64 {
        self.recv.last_accessed()
    }
    fn last_modified(&self) -> u64 {
        self.recv.last_modified()
    }
    fn created_time(&self) -> u64 {
        self.recv.created_time()
    }
    fn size(&self) -> u64 {
        self.recv.size()
    }
    fn set_len(&mut self, i: u64) -> Result<(), FsError> {
        self.recv.set_len(i)
    }
    fn unlink(&mut self) -> Result<(), FsError> {
        self.recv.unlink()
    }
    fn bytes_available_read(&self) -> Result<Option<usize>, FsError> {
        self.recv.bytes_available_read()
    }
}

impl Default for WasiBidirectionalPipePair {
    fn default() -> Self {
        Self::new()
    }
}

impl WasiBidirectionalPipePair {
    pub fn new() -> WasiBidirectionalPipePair {
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();

        let pipe1 = WasiPipe {
            tx: Mutex::new(tx1),
            rx: Mutex::new(rx2),
            read_buffer: Mutex::new(None),
            block: true,
        };

        let pipe2 = WasiPipe {
            tx: Mutex::new(tx2),
            rx: Mutex::new(rx1),
            read_buffer: Mutex::new(None),
            block: true,
        };

        WasiBidirectionalPipePair {
            send: pipe1,
            recv: pipe2,
        }
    }

    #[allow(dead_code)]
    pub fn with_blocking(mut self, block: bool) -> Self {
        self.set_blocking(block);
        self
    }

    /// Whether to block on reads (ususally for waiting for stdin keyboard input). Default: `true`
    #[allow(dead_code)]
    pub fn set_blocking(&mut self, block: bool) {
        self.send.set_blocking(block);
        self.recv.set_blocking(block);
    }
}

/// Shared version of WasiBidirectionalPipePair for situations where you need
/// to emulate the old behaviour of `Pipe` (both send and recv on one channel).
#[derive(Debug, Clone)]
pub struct WasiBidirectionalSharedPipePair {
    inner: Arc<Mutex<WasiBidirectionalPipePair>>,
}

impl Default for WasiBidirectionalSharedPipePair {
    fn default() -> Self {
        Self::new()
    }
}

impl WasiBidirectionalSharedPipePair {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(WasiBidirectionalPipePair::new())),
        }
    }

    #[allow(dead_code)]
    pub fn with_blocking(mut self, block: bool) -> Self {
        self.set_blocking(block);
        self
    }

    /// Whether to block on reads (ususally for waiting for stdin keyboard input). Default: `true`
    #[allow(dead_code)]
    pub fn set_blocking(&mut self, block: bool) {
        self.inner.lock().unwrap().set_blocking(block);
    }
}

impl Write for WasiBidirectionalSharedPipePair {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.inner.lock().as_mut().map(|l| l.write(buf)) {
            Ok(r) => r,
            Err(_) => Ok(0),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self.inner.lock().as_mut().map(|l| l.flush()) {
            Ok(r) => r,
            Err(_) => Ok(()),
        }
    }
}

impl Seek for WasiBidirectionalSharedPipePair {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Ok(0)
    }
}

impl Read for WasiBidirectionalSharedPipePair {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.inner.lock().as_mut().map(|l| l.read(buf)) {
            Ok(r) => r,
            Err(_) => Ok(0),
        }
    }
}

impl VirtualFile for WasiBidirectionalSharedPipePair {
    fn last_accessed(&self) -> u64 {
        self.inner.lock().map(|l| l.last_accessed()).unwrap_or(0)
    }
    fn last_modified(&self) -> u64 {
        self.inner.lock().map(|l| l.last_modified()).unwrap_or(0)
    }
    fn created_time(&self) -> u64 {
        self.inner.lock().map(|l| l.created_time()).unwrap_or(0)
    }
    fn size(&self) -> u64 {
        self.inner.lock().map(|l| l.size()).unwrap_or(0)
    }
    fn set_len(&mut self, i: u64) -> Result<(), FsError> {
        match self.inner.lock().as_mut().map(|l| l.set_len(i)) {
            Ok(r) => r,
            Err(_) => Err(FsError::Lock),
        }
    }
    fn unlink(&mut self) -> Result<(), FsError> {
        match self.inner.lock().as_mut().map(|l| l.unlink()) {
            Ok(r) => r,
            Err(_) => Err(FsError::Lock),
        }
    }
    fn bytes_available_read(&self) -> Result<Option<usize>, FsError> {
        self.inner
            .lock()
            .map(|l| l.bytes_available_read())
            .unwrap_or(Ok(None))
    }
}

impl WasiPipe {
    /// Same as `set_blocking`, but as a builder method
    pub fn with_blocking(mut self, block: bool) -> Self {
        self.set_blocking(block);
        self
    }

    /// Whether to block on reads (ususally for waiting for stdin keyboard input). Default: `true`
    pub fn set_blocking(&mut self, block: bool) {
        self.block = block;
    }

    pub fn recv<M: MemorySize>(
        &mut self,
        memory: &MemoryView,
        iov: WasmSlice<__wasi_iovec_t<M>>,
        timeout: Duration,
    ) -> Result<usize, Errno> {
        let mut elapsed = Duration::ZERO;
        let mut tick_wait = 0u64;
        loop {
            {
                let mut read_buffer = self.read_buffer.lock().unwrap();
                if let Some(buf) = read_buffer.as_mut() {
                    let buf_len = buf.len();
                    if buf_len > 0 {
                        let reader = buf.as_ref();
                        let read = read_bytes(reader, memory, iov).map(|a| a as usize)?;
                        buf.advance(read);
                        return Ok(read);
                    }
                }
            }
            let rx = self.rx.lock().unwrap();
            let data = match rx.try_recv() {
                Ok(a) => a,
                Err(TryRecvError::Empty) => {
                    if elapsed > timeout {
                        return Err(Errno::Timedout);
                    }
                    // Linearly increasing wait time
                    tick_wait += 1;
                    let wait_time = u64::min(tick_wait / 10, 20);
                    let wait_time = std::time::Duration::from_millis(wait_time);
                    std::thread::park_timeout(wait_time);
                    elapsed += wait_time;
                    continue;
                }
                Err(TryRecvError::Disconnected) => {
                    return Ok(0);
                }
            };
            drop(rx);

            // FIXME: this looks like a race condition!
            let mut read_buffer = self.read_buffer.lock().unwrap();
            read_buffer.replace(Bytes::from(data));
        }
    }

    pub fn send<M: MemorySize>(
        &mut self,
        memory: &MemoryView,
        iov: WasmSlice<__wasi_ciovec_t<M>>,
    ) -> Result<usize, Errno> {
        let buf_len: M::Offset = iov
            .iter()
            .filter_map(|a| a.read().ok())
            .map(|a| a.buf_len)
            .sum();
        let buf_len: usize = buf_len.try_into().map_err(|_| Errno::Inval)?;
        let mut buf = Vec::with_capacity(buf_len);
        write_bytes(&mut buf, memory, iov)?;
        let tx = self.tx.lock().unwrap();
        tx.send(buf).map_err(|_| Errno::Io)?;
        Ok(buf_len)
    }

    pub fn close(&mut self) {
        let (mut null_tx, _) = mpsc::channel();
        let (_, mut null_rx) = mpsc::channel();
        {
            let mut guard = self.rx.lock().unwrap();
            std::mem::swap(guard.deref_mut(), &mut null_rx);
        }
        {
            let mut guard = self.tx.lock().unwrap();
            std::mem::swap(guard.deref_mut(), &mut null_tx);
        }
        {
            let mut read_buffer = self.read_buffer.lock().unwrap();
            read_buffer.take();
        }
    }
}

impl Write for WasiPipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let buf_len = buf.len();
        let tx = self.tx.lock().unwrap();
        tx.send(buf.to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{e}")))?;
        Ok(buf_len)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for WasiPipe {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Ok(0)
    }
}

impl Read for WasiPipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let mut read_buffer = self.read_buffer.lock().unwrap();
            if let Some(inner_buf) = read_buffer.as_mut() {
                let buf_len = inner_buf.len();
                if buf_len > 0 {
                    if inner_buf.len() > buf.len() {
                        let mut reader = inner_buf.as_ref();
                        let read = reader.read_exact(buf).map(|_| buf.len())?;
                        inner_buf.advance(read);
                        return Ok(read);
                    } else {
                        let mut reader = inner_buf.as_ref();
                        let read = reader.read(buf).map(|_| buf_len as usize)?;
                        inner_buf.advance(read);
                        return Ok(read);
                    }
                }
            }
            let rx = self.rx.lock().unwrap();

            // We need to figure out whether we need to block here.
            // The problem is that in cases of multiple buffered reads like:
            //
            // println!("abc");
            // println!("def");
            //
            // get_stdout() // would only return "abc\n" instead of "abc\ndef\n"

            let data = match rx.try_recv() {
                Ok(mut s) => {
                    s.append(&mut rx.try_iter().flat_map(|f| f.into_iter()).collect());
                    s
                }
                Err(_) => {
                    if !self.block {
                        // If self.block is explicitly set to false, never block
                        Vec::new()
                    } else {
                        // could not immediately receive bytes, so we need to block
                        match rx.recv() {
                            Ok(o) => o,
                            // Errors can happen if the sender has been dropped already
                            // In this case, just return 0 to indicate that we can't read any
                            // bytes anymore
                            Err(_) => {
                                return Ok(0);
                            }
                        }
                    }
                }
            };

                let mut read_buffer = self.read_buffer.lock().unwrap();
                if data.is_empty() && read_buffer.lock().unwrap().as_ref().map(|s| s.len()).unwrap_or(0) == 0 {
                    return Ok(0);
                }
                read_buffer.replace(Bytes::from(data));
        }
    }
}

impl std::io::Write for WasiPipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let tx = self.tx.lock().unwrap();
        tx.send(buf.to_vec())
            .map_err(|_| Into::<std::io::Error>::into(std::io::ErrorKind::BrokenPipe))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl VirtualFile for WasiPipe {
    fn last_accessed(&self) -> u64 {
        0
    }
    fn last_modified(&self) -> u64 {
        0
    }
    fn created_time(&self) -> u64 {
        0
    }
    fn size(&self) -> u64 {
        self.read_buffer
            .as_ref()
            .map(|s| s.len() as u64)
            .unwrap_or_default()
    }
    fn set_len(&mut self, _: u64) -> Result<(), FsError> {
        Ok(())
    }
    fn unlink(&mut self) -> Result<(), FsError> {
        Ok(())
    }
    fn bytes_available_read(&self) -> Result<Option<usize>, FsError> {
        Ok(Some(
            self.read_buffer
                .as_ref()
                .map(|s| s.len())
                .unwrap_or_default(),
        ))
    }
}

impl VirtualFile for WasiPipe {
    /// the last time the file was accessed in nanoseconds as a UNIX timestamp
    fn last_accessed(&self) -> u64 {
        0
    }

    /// the last time the file was modified in nanoseconds as a UNIX timestamp
    fn last_modified(&self) -> u64 {
        0
    }

    /// the time at which the file was created in nanoseconds as a UNIX timestamp
    fn created_time(&self) -> u64 {
        0
    }

    /// the size of the file in bytes
    fn size(&self) -> u64 {
        0
    }

    /// Change the size of the file, if the `new_size` is greater than the current size
    /// the extra bytes will be allocated and zeroed
    fn set_len(&mut self, _new_size: u64) -> Result<(), FsError> {
        Ok(())
    }

    /// Request deletion of the file
    fn unlink(&mut self) -> Result<(), FsError> {
        Ok(())
    }

    /// Store file contents and metadata to disk
    /// Default implementation returns `Ok(())`.  You should implement this method if you care
    /// about flushing your cache to permanent storage
    fn sync_to_disk(&self) -> Result<(), FsError> {
        Ok(())
    }

    /// Returns the number of bytes available.  This function must not block
    fn bytes_available(&self) -> Result<usize, FsError> {
        Ok(self.bytes_available_read()?.unwrap_or(0usize)
            + self.bytes_available_write()?.unwrap_or(0usize))
    }

    /// Returns the number of bytes available.  This function must not block
    /// Defaults to `None` which means the number of bytes is unknown
    fn bytes_available_read(&self) -> Result<Option<usize>, FsError> {
        loop {
            {
                let read_buffer = self.read_buffer.lock().unwrap();
                if let Some(inner_buf) = read_buffer.as_ref() {
                    let buf_len = inner_buf.len();
                    if buf_len > 0 {
                        return Ok(Some(buf_len));
                    }
                }
            }
            let rx = self.rx.lock().unwrap();
            // FIXME: why is a bytes available check consuming data? - this shouldn't be necessary
            if let Ok(data) = rx.try_recv() {
                drop(rx);

                let mut read_buffer = self.read_buffer.lock().unwrap();
                read_buffer.replace(Bytes::from(data));
            } else {
                return Ok(Some(0));
            }
        }
    }

    /// Returns the number of bytes available.  This function must not block
    /// Defaults to `None` which means the number of bytes is unknown
    fn bytes_available_write(&self) -> Result<Option<usize>, FsError> {
        Ok(None)
    }

    /// Indicates if the file is opened or closed. This function must not block
    /// Defaults to a status of being constantly open
    fn is_open(&self) -> bool {
        true
    }

    /// Returns a special file descriptor when opening this file rather than
    /// generating a new one
    fn get_special_fd(&self) -> Option<u32> {
        None
    }

    /// Used for polling.  Default returns `None` because this method cannot be implemented for most types
    /// Returns the underlying host fd
    fn get_fd(&self) -> Option<wasmer_vfs::FileDescriptor> {
        None
    }
}
