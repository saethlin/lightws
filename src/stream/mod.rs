//! Websocket stream.
//!
//! [`Stream`] is a simple wrapper of the underlying IO source,
//! with small stack buffers to save states.
//!
//! It is transparent to call `Read` or `Write` on Stream:
//!
//! ```ignore
//! {
//!     // establish connection, handshake
//!     let stream = ...
//!     // read some data
//!     stream.read(&mut buf)?;
//!     // write some data
//!     stream.write(&buf)?;
//! }
//! ```
//!
//! One `Read` or `Write` leads to **at most One**
//! operation(or syscall) on the underlying IO source.
//! Stream itself does not buffer any payload data during
//! a `Read` or `Write`, so there is no extra heap allocation.

mod read;
mod write;

mod state;
mod detail;
mod special;

cfg_if::cfg_if! {
    if #[cfg(feature = "tokio")] {
        mod async_read;
        mod async_write;
    }
}

use std::marker::PhantomData;
use state::{ReadState, WriteState, HeartBeat};
use crate::role::RoleHelper;

/// Websocket stream.
///
/// Depending on `IO`, [`Stream`] implements [`std::io::Read`] and [`std::io::Write`]
/// or [`tokio::io::AsyncRead`] and [`tokio::io::AsyncWrite`].
///
/// `Role` decides whether to mask payload data.
/// It is reserved to provide extra infomation to apply optimizations.
///
/// See also: `Stream::read`, `Stream::write`.
pub struct Stream<IO, Role> {
    io: IO,
    read_state: ReadState,
    write_state: WriteState,
    heartbeat: HeartBeat,
    _marker: PhantomData<Role>,
}

impl<IO, Role> AsRef<IO> for Stream<IO, Role> {
    #[inline]
    fn as_ref(&self) -> &IO { &self.io }
}

impl<IO, Role> AsMut<IO> for Stream<IO, Role> {
    #[inline]
    fn as_mut(&mut self) -> &mut IO { &mut self.io }
}

impl<IO, Role> std::fmt::Debug for Stream<IO, Role> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("read_state", &self.read_state)
            .field("write_state", &self.write_state)
            .field("heartbeat", &self.heartbeat)
            .finish()
    }
}

impl<IO, Role> Stream<IO, Role> {
    /// Create websocket stream from IO source directly,
    /// without a handshake.
    #[inline]
    pub const fn new(io: IO) -> Self {
        Stream {
            io,
            read_state: ReadState::new(),
            write_state: WriteState::new(),
            heartbeat: HeartBeat::new(),
            _marker: PhantomData,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::{Read, Write, Result};
    use crate::frame::*;
    use crate::role::*;

    pub struct LimitReadWriter {
        pub buf: Vec<u8>,
        pub rlimit: usize,
        pub wlimit: usize,
        pub cursor: usize,
    }

    impl Read for LimitReadWriter {
        fn read(&mut self, mut buf: &mut [u8]) -> Result<usize> {
            let to_read = std::cmp::min(buf.len(), self.rlimit);
            let left_data = self.buf.len() - self.cursor;
            if left_data == 0 {
                return Ok(0);
            }
            if left_data <= to_read {
                buf.write(&self.buf[self.cursor..]).unwrap();
                self.cursor = self.buf.len();
                return Ok(left_data);
            }

            buf.write(&self.buf[self.cursor..self.cursor + to_read])
                .unwrap();
            self.cursor += to_read;
            Ok(to_read)
        }
    }

    impl Write for LimitReadWriter {
        fn write(&mut self, buf: &[u8]) -> Result<usize> {
            let len = std::cmp::min(buf.len(), self.wlimit);
            self.buf.write(&buf[..len])
        }

        fn flush(&mut self) -> Result<()> { Ok(()) }
    }

    pub fn make_frame<R: RoleHelper>(opcode: OpCode, len: usize) -> (Vec<u8>, Vec<u8>) {
        let data: Vec<u8> = std::iter::repeat(rand::random::<u8>()).take(len).collect();
        let mut data2 = data.clone();

        let mut tmp = vec![0; 14];
        let head = FrameHead::new(
            Fin::Y,
            opcode,
            R::new_write_mask(),
            PayloadLen::from_num(len as u64),
        );

        let head_len = head.encode(&mut tmp).unwrap();
        let mut frame = Vec::new();
        let write_n = frame.write(&tmp[..head_len]).unwrap();
        assert_eq!(write_n, head_len);

        frame.append(&mut data2);
        assert_eq!(frame.len(), len + head_len);

        (frame, data)
    }

    #[test]
    fn read_write_stream() {
        fn read_write<R: RoleHelper>(rlimit: usize, wlimit: usize, len: usize) {
            let io = LimitReadWriter {
                buf: Vec::new(),
                rlimit,
                wlimit,
                cursor: 0,
            };
            // data written to a client stream should be read as a server stream.
            // here we read/write on the same (client/server)stream.
            // this is not correct in practice, but our program can still handle it.
            let mut stream = Stream::<_, R>::new(io);

            let data: Vec<u8> = std::iter::repeat(rand::random::<u8>()).take(len).collect();
            let mut data2: Vec<u8> = Vec::new();

            let mut buf = vec![0; 0x2000];
            let mut to_write = data.len();

            while to_write > 0 {
                let wbeg = data.len() - to_write;
                let n = loop {
                    let x = stream.write(&data[wbeg..]).unwrap();
                    if x != 0 {
                        break x;
                    }
                };

                let mut tmp: Vec<u8> = Vec::new();
                loop {
                    // avoid read EOF here
                    if stream.as_ref().cursor == stream.as_ref().buf.len() {
                        break;
                    }
                    let n = stream.read(&mut buf).unwrap();

                    // if n == 0 && stream.is_read_end() {
                    //     break;
                    // }

                    tmp.write(&buf[..n]).unwrap();
                }

                assert_eq!(tmp.len(), n);
                assert_eq!(&data[wbeg..wbeg + n], &tmp);

                to_write -= n;
                data2.append(&mut tmp);
            }

            assert_eq!(&data, &data2);
        }

        for limit in 1..512 {
            for len in 1..=256 {
                read_write::<Client>(limit, 512 - limit, len);
                read_write::<Server>(limit, 512 - limit, len);
            }
        }
    }
}
