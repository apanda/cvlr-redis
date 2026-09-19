//! A minimal blocking RESP2 client. std only -- this crate has no network dependencies and
//! `redis-cli` is not built in this tree.

use std::io::{BufReader, BufWriter, Read, Write};
use std::net::TcpStream;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resp {
    Simple(String),
    Error(String),
    Int(i64),
    Bulk(Vec<u8>),
    Nil,
    Array(Vec<Resp>),
    NilArray,
}

impl Resp {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Resp::Int(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_bulk(&self) -> Option<&[u8]> {
        match self {
            Resp::Bulk(b) => Some(b),
            _ => None,
        }
    }
    pub fn is_nil(&self) -> bool {
        matches!(self, Resp::Nil | Resp::NilArray)
    }
}

pub struct Client {
    r: BufReader<TcpStream>,
    w: BufWriter<TcpStream>,
}

impl Client {
    pub fn connect(port: u16) -> std::io::Result<Self> {
        let s = TcpStream::connect(("127.0.0.1", port))?;
        s.set_nodelay(true)?;
        Ok(Client { r: BufReader::new(s.try_clone()?), w: BufWriter::new(s) })
    }

    pub fn cmd(&mut self, args: &[&[u8]]) -> std::io::Result<Resp> {
        write!(self.w, "*{}\r\n", args.len())?;
        for a in args {
            write!(self.w, "${}\r\n", a.len())?;
            self.w.write_all(a)?;
            self.w.write_all(b"\r\n")?;
        }
        self.w.flush()?;
        self.read()
    }

    /// Convenience for all-text commands.
    pub fn cmd_s(&mut self, args: &[&str]) -> std::io::Result<Resp> {
        let owned: Vec<&[u8]> = args.iter().map(|s| s.as_bytes()).collect();
        self.cmd(&owned)
    }

    /// Become a replica: send SYNC and consume the RDB payload, leaving the socket
    /// positioned at the start of the propagated command stream.
    ///
    /// This is Redis's own `attach_to_replication_stream`
    /// (tests/test_helper.tcl:787-864). The payload arrives as `$<len>\r\n<bytes>` with
    /// NO trailing CRLF -- unlike a normal bulk string -- so it must be skipped by length
    /// rather than parsed.
    pub fn sync_start(&mut self) -> std::io::Result<usize> {
        self.w.write_all(b"SYNC\r\n")?;
        self.w.flush()?;
        let hdr = self.read_line()?;
        if hdr.first() != Some(&b'$') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("SYNC did not return a bulk payload: {:?}", String::from_utf8_lossy(&hdr)),
            ));
        }
        let n: usize = String::from_utf8_lossy(&hdr[1..]).trim().parse().unwrap_or(0);
        let mut buf = vec![0u8; n];
        self.r.read_exact(&mut buf)?;
        Ok(n)
    }

    /// Read propagated commands until the stream goes quiet for `quiet_ms`.
    ///
    /// Returns each command as its argument vector. `SELECT` and `PING` are filtered:
    /// the master emits `SELECT` once at stream start and `PING`s on a timer, and neither
    /// is an effect of anything the test did.
    pub fn drain_propagated(&mut self, quiet_ms: u64) -> std::io::Result<Vec<Vec<String>>> {
        self.r.get_ref().set_read_timeout(Some(std::time::Duration::from_millis(quiet_ms)))?;
        let mut out = Vec::new();
        loop {
            match self.read() {
                Ok(Resp::Array(items)) => {
                    let args: Vec<String> = items
                        .iter()
                        .map(|r| match r {
                            Resp::Bulk(b) => String::from_utf8_lossy(b).to_string(),
                            other => format!("{other:?}"),
                        })
                        .collect();
                    if let Some(first) = args.first() {
                        let up = first.to_uppercase();
                        if up == "SELECT" || up == "PING" || up == "REPLCONF" {
                            continue;
                        }
                    }
                    out.push(args);
                }
                Ok(_) => continue,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break
                }
                Err(e) => return Err(e),
            }
        }
        self.r.get_ref().set_read_timeout(None)?;
        Ok(out)
    }

    fn read_line(&mut self) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        loop {
            self.r.read_exact(&mut b)?;
            if b[0] == b'\r' {
                self.r.read_exact(&mut b)?; // consume \n
                return Ok(out);
            }
            out.push(b[0]);
        }
    }

    fn read(&mut self) -> std::io::Result<Resp> {
        let line = self.read_line()?;
        if line.is_empty() {
            return Ok(Resp::Nil);
        }
        let (tag, rest) = (line[0], &line[1..]);
        let text = String::from_utf8_lossy(rest).to_string();
        Ok(match tag {
            b'+' => Resp::Simple(text),
            b'-' => Resp::Error(text),
            b':' => Resp::Int(text.parse().unwrap_or(0)),
            b'$' => {
                let n: i64 = text.parse().unwrap_or(-1);
                if n < 0 {
                    Resp::Nil
                } else {
                    let mut buf = vec![0u8; n as usize + 2];
                    self.r.read_exact(&mut buf)?;
                    buf.truncate(n as usize);
                    Resp::Bulk(buf)
                }
            }
            b'*' => {
                let n: i64 = text.parse().unwrap_or(-1);
                if n < 0 {
                    Resp::NilArray
                } else {
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(self.read()?);
                    }
                    Resp::Array(v)
                }
            }
            _ => Resp::Simple(text),
        })
    }
}
