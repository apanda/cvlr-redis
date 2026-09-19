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
