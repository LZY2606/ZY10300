//! Thrift compact protocol reader/writer for the subset used by Parquet metadata.
use anyhow::{bail, Result};

pub const T_STOP: u8 = 0;
pub const T_TRUE: u8 = 1;
pub const T_FALSE: u8 = 2;
pub const T_BYTE: u8 = 3;
pub const T_I16: u8 = 4;
pub const T_I32: u8 = 5;
pub const T_I64: u8 = 6;
pub const T_DOUBLE: u8 = 7;
pub const T_BINARY: u8 = 8;
pub const T_LIST: u8 = 9;
pub const T_SET: u8 = 10;
pub const T_MAP: u8 = 11;
pub const T_STRUCT: u8 = 12;

#[derive(Debug, Clone, PartialEq)]
pub enum TVal {
    Bool(bool),
    Byte(i8),
    I32(i32),
    I64(i64),
    Double(f64),
    Binary(Vec<u8>),
    List(Vec<TVal>),
    Struct(Vec<(i16, TVal)>),
}

impl TVal {
    pub fn str(s: &str) -> TVal {
        TVal::Binary(s.as_bytes().to_vec())
    }
    pub fn get<'a>(fields: &'a [(i16, TVal)], id: i16) -> Option<&'a TVal> {
        fields.iter().find(|(fid, _)| *fid == id).map(|(_, v)| v)
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            TVal::I32(v) => Some(*v as i64),
            TVal::I64(v) => Some(*v),
            TVal::Byte(v) => Some(*v as i64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            TVal::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_bin(&self) -> Option<&[u8]> {
        match self {
            TVal::Binary(b) => Some(b),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<String> {
        self.as_bin()
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }
    pub fn as_list(&self) -> Option<&[TVal]> {
        match self {
            TVal::List(l) => Some(l),
            _ => None,
        }
    }
    pub fn as_struct(&self) -> Option<&[(i16, TVal)]> {
        match self {
            TVal::Struct(s) => Some(s),
            _ => None,
        }
    }
}

pub struct TReader<'a> {
    buf: &'a [u8],
    pub pos: usize,
}

impl<'a> TReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        TReader { buf, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn byte(&mut self) -> Result<u8> {
        if self.pos >= self.buf.len() {
            bail!("thrift: unexpected end of input at {}", self.pos);
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            bail!("thrift: need {} bytes, have {}", n, self.remaining());
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn varint(&mut self) -> Result<u64> {
        let mut shift = 0u32;
        let mut out = 0u64;
        loop {
            let b = self.byte()?;
            out |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(out);
            }
            shift += 7;
            if shift >= 64 {
                bail!("thrift: varint too long");
            }
        }
    }
    pub fn zigzag(&mut self) -> Result<i64> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    pub fn read_struct(&mut self) -> Result<Vec<(i16, TVal)>> {
        let mut fields = Vec::new();
        let mut last_id: i16 = 0;
        loop {
            let b = self.byte()?;
            if b == T_STOP {
                break;
            }
            let delta = (b >> 4) & 0x0f;
            let t = b & 0x0f;
            let id = if delta == 0 {
                self.zigzag()? as i16
            } else {
                last_id + delta as i16
            };
            last_id = id;
            let v = self.read_value(t, false)?;
            fields.push((id, v));
        }
        Ok(fields)
    }
    fn read_value(&mut self, t: u8, in_collection: bool) -> Result<TVal> {
        Ok(match t {
            T_TRUE | T_FALSE => {
                if in_collection {
                    // collection bools carry a payload byte
                    TVal::Bool(self.byte()? == T_TRUE)
                } else {
                    TVal::Bool(t == T_TRUE)
                }
            }
            T_BYTE => TVal::Byte(self.byte()? as i8),
            T_I16 | T_I32 => TVal::I32(self.zigzag()? as i32),
            T_I64 => TVal::I64(self.zigzag()?),
            T_DOUBLE => {
                let b = self.take(8)?;
                TVal::Double(f64::from_le_bytes(b.try_into().unwrap()))
            }
            T_BINARY => {
                let n = self.varint()? as usize;
                TVal::Binary(self.take(n)?.to_vec())
            }
            T_LIST | T_SET => {
                let h = self.byte()?;
                let mut size = (h >> 4) as usize & 0x0f;
                let et = h & 0x0f;
                if size == 15 {
                    size = self.varint()? as usize;
                }
                if size > 1 << 24 {
                    bail!("thrift: list size {} too large", size);
                }
                let mut items = Vec::with_capacity(size.min(1 << 16));
                for _ in 0..size {
                    items.push(self.read_value(et, true)?);
                }
                TVal::List(items)
            }
            T_MAP => {
                // Parsed for skip-ability only; represented as list of 2-lists.
                let size = self.varint()? as usize;
                if size > 1 << 24 {
                    bail!("thrift: map size {} too large", size);
                }
                let mut items = Vec::new();
                if size > 0 {
                    let kv = self.byte()?;
                    let (kt, vt) = (kv >> 4, kv & 0x0f);
                    for _ in 0..size {
                        let k = self.read_value(kt, true)?;
                        let v = self.read_value(vt, true)?;
                        items.push(TVal::List(vec![k, v]));
                    }
                }
                TVal::List(items)
            }
            T_STRUCT => TVal::Struct(self.read_struct()?),
            _ => bail!("thrift: unknown type {}", t),
        })
    }
}

fn varint_into(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

fn zigzag_into(v: i64, out: &mut Vec<u8>) {
    varint_into(((v << 1) ^ (v >> 63)) as u64, out)
}

fn tval_type(v: &TVal) -> u8 {
    match v {
        TVal::Bool(_) => T_TRUE,
        TVal::Byte(_) => T_BYTE,
        TVal::I32(_) => T_I32,
        TVal::I64(_) => T_I64,
        TVal::Double(_) => T_DOUBLE,
        TVal::Binary(_) => T_BINARY,
        TVal::List(_) => T_LIST,
        TVal::Struct(_) => T_STRUCT,
    }
}

fn write_value(v: &TVal, in_collection: bool, out: &mut Vec<u8>) {
    match v {
        TVal::Bool(b) => {
            if in_collection {
                out.push(if *b { T_TRUE } else { T_FALSE });
            }
        }
        TVal::Byte(b) => out.push(*b as u8),
        TVal::I32(v) => zigzag_into(*v as i64, out),
        TVal::I64(v) => zigzag_into(*v, out),
        TVal::Double(d) => out.extend_from_slice(&d.to_le_bytes()),
        TVal::Binary(b) => {
            varint_into(b.len() as u64, out);
            out.extend_from_slice(b);
        }
        TVal::List(items) => {
            let et = items.first().map(tval_type).unwrap_or(T_STOP);
            if items.len() < 15 {
                out.push(((items.len() as u8) << 4) | et);
            } else {
                out.push(0xf0 | et);
                varint_into(items.len() as u64, out);
            }
            for it in items {
                write_value(it, true, out);
            }
        }
        TVal::Struct(fields) => write_struct_into(fields, out),
    }
}

pub fn write_struct_into(fields: &[(i16, TVal)], out: &mut Vec<u8>) {
    let mut last: i16 = 0;
    for (id, v) in fields {
        let t = match v {
            TVal::Bool(b) => {
                if *b {
                    T_TRUE
                } else {
                    T_FALSE
                }
            }
            other => tval_type(other),
        };
        let delta = *id - last;
        if delta > 0 && delta <= 15 {
            out.push(((delta as u8) << 4) | t);
        } else {
            out.push(t);
            zigzag_into(*id as i64, out);
        }
        last = *id;
        write_value(v, false, out);
    }
    out.push(T_STOP);
}

pub fn write_struct(fields: &[(i16, TVal)]) -> Vec<u8> {
    let mut out = Vec::new();
    write_struct_into(fields, &mut out);
    out
}
