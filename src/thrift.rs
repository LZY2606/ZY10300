//! Minimal Thrift compact protocol reader/writer.
//! Only what Parquet metadata needs: structs, lists, enums(i32), strings, bools.

pub type Res<T> = Result<T, String>;

pub mod ctype {
    pub const STOP: u8 = 0x00;
    pub const TRUE: u8 = 0x01;
    pub const FALSE: u8 = 0x02;
    pub const BYTE: u8 = 0x03;
    pub const I16: u8 = 0x04;
    pub const I32: u8 = 0x05;
    pub const I64: u8 = 0x06;
    pub const DOUBLE: u8 = 0x07;
    pub const BINARY: u8 = 0x08;
    pub const LIST: u8 = 0x09;
    pub const SET: u8 = 0x0A;
    pub const MAP: u8 = 0x0B;
    pub const STRUCT: u8 = 0x0C;
}

pub struct TReader<'a> {
    buf: &'a [u8],
    pos: usize,
    id_stack: Vec<i16>,
}

impl<'a> TReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        TReader { buf, pos: 0, id_stack: vec![0] }
    }
    pub fn position(&self) -> usize {
        self.pos
    }
    fn byte(&mut self) -> Res<u8> {
        let b = *self.buf.get(self.pos).ok_or("thrift: 意外结束(读字节)")?;
        self.pos += 1;
        Ok(b)
    }
    pub fn varint(&mut self) -> Res<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            result |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift > 63 {
                return Err("thrift: varint 过长".into());
            }
        }
    }
    pub fn zigzag(&mut self) -> Res<i64> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    pub fn struct_enter(&mut self) {
        self.id_stack.push(0);
    }
    pub fn struct_exit(&mut self) {
        self.id_stack.pop();
    }
    /// Returns Ok(None) on STOP.
    pub fn field_begin(&mut self) -> Res<Option<(i16, u8)>> {
        let b = self.byte()?;
        if b == ctype::STOP {
            return Ok(None);
        }
        let ty = b & 0x0f;
        let delta = (b >> 4) & 0x0f;
        let last = *self.id_stack.last().unwrap_or(&0);
        let id = if delta == 0 {
            self.zigzag()? as i16
        } else {
            last + delta as i16
        };
        if let Some(top) = self.id_stack.last_mut() {
            *top = id;
        }
        Ok(Some((id, ty)))
    }
    pub fn list_begin(&mut self) -> Res<(usize, u8)> {
        let b = self.byte()?;
        let etype = b & 0x0f;
        let mut size = ((b >> 4) & 0x0f) as usize;
        if size == 15 {
            size = self.varint()? as usize;
        }
        Ok((size, etype))
    }
    pub fn binary(&mut self) -> Res<Vec<u8>> {
        let len = self.varint()? as usize;
        if self.pos + len > self.buf.len() {
            return Err("thrift: binary 越界".into());
        }
        let out = self.buf[self.pos..self.pos + len].to_vec();
        self.pos += len;
        Ok(out)
    }
    pub fn string(&mut self) -> Res<String> {
        let b = self.binary()?;
        String::from_utf8(b).map_err(|_| "thrift: 非 UTF-8 字符串".to_string())
    }
    pub fn i32v(&mut self) -> Res<i32> {
        Ok(self.zigzag()? as i32)
    }
    pub fn i64v(&mut self) -> Res<i64> {
        self.zigzag()
    }
    pub fn bool_value(&mut self, field_type: u8) -> Res<bool> {
        match field_type {
            ctype::TRUE => Ok(true),
            ctype::FALSE => Ok(false),
            _ => Err(format!("thrift: 非法布尔字段类型 {field_type}")),
        }
    }
    pub fn skip(&mut self, ty: u8) -> Res<()> {
        match ty {
            ctype::TRUE | ctype::FALSE => Ok(()),
            ctype::BYTE => {
                self.byte()?;
                Ok(())
            }
            ctype::I16 | ctype::I32 | ctype::I64 => {
                self.varint()?;
                Ok(())
            }
            ctype::DOUBLE => {
                for _ in 0..8 {
                    self.byte()?;
                }
                Ok(())
            }
            ctype::BINARY => {
                self.binary()?;
                Ok(())
            }
            ctype::LIST | ctype::SET => {
                let (n, et) = self.list_begin()?;
                for _ in 0..n {
                    self.skip(et)?;
                }
                Ok(())
            }
            ctype::MAP => {
                let b = self.byte()?;
                if b != 0 {
                    let size = self.varint()? as usize;
                    let kt = b >> 4;
                    let vt = b & 0x0f;
                    for _ in 0..size {
                        self.skip(kt)?;
                        self.skip(vt)?;
                    }
                }
                Ok(())
            }
            ctype::STRUCT => {
                self.struct_enter();
                while let Some((_, ft)) = self.field_begin()? {
                    self.skip(ft)?;
                }
                self.struct_exit();
                Ok(())
            }
            _ => Err(format!("thrift: 无法跳过未知类型 {ty}")),
        }
    }
}

#[derive(Default)]
pub struct TWriter {
    pub buf: Vec<u8>,
    id_stack: Vec<i16>,
}

impl TWriter {
    pub fn new() -> Self {
        TWriter { buf: Vec::new(), id_stack: vec![0] }
    }
    fn varint(&mut self, mut v: u64) {
        loop {
            if v < 0x80 {
                self.buf.push(v as u8);
                return;
            }
            self.buf.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
    }
    fn zigzag(&mut self, v: i64) {
        self.varint(((v << 1) ^ (v >> 63)) as u64)
    }
    pub fn struct_enter(&mut self) {
        self.id_stack.push(0);
    }
    pub fn struct_exit(&mut self) {
        self.buf.push(ctype::STOP);
        self.id_stack.pop();
    }
    fn field_header(&mut self, id: i16, ty: u8) {
        let last = *self.id_stack.last().unwrap_or(&0);
        let delta = id - last;
        if (1..=15).contains(&delta) {
            self.buf.push(((delta as u8) << 4) | ty);
        } else {
            self.buf.push(ty);
            self.zigzag(id as i64);
        }
        if let Some(top) = self.id_stack.last_mut() {
            *top = id;
        }
    }
    pub fn field_i32(&mut self, id: i16, v: i32) {
        self.field_header(id, ctype::I32);
        self.zigzag(v as i64);
    }
    pub fn field_i64(&mut self, id: i16, v: i64) {
        self.field_header(id, ctype::I64);
        self.zigzag(v);
    }
    pub fn field_bool(&mut self, id: i16, v: bool) {
        self.field_header(id, if v { ctype::TRUE } else { ctype::FALSE });
    }
    pub fn field_binary(&mut self, id: i16, b: &[u8]) {
        self.field_header(id, ctype::BINARY);
        self.binary_raw(b);
    }
    pub fn field_string(&mut self, id: i16, s: &str) {
        self.field_binary(id, s.as_bytes());
    }
    pub fn binary_raw(&mut self, b: &[u8]) {
        self.varint(b.len() as u64);
        self.buf.extend_from_slice(b);
    }
    pub fn list_field_begin(&mut self, id: i16, elem_ty: u8, len: usize) {
        self.field_header(id, ctype::LIST);
        self.list_header(elem_ty, len);
    }
    pub fn list_header(&mut self, elem_ty: u8, len: usize) {
        if len <= 14 {
            self.buf.push(((len as u8) << 4) | elem_ty);
        } else {
            self.buf.push(0xf0 | elem_ty);
            self.varint(len as u64);
        }
    }
    pub fn i32_raw(&mut self, v: i32) {
        self.zigzag(v as i64);
    }
    pub fn i64_raw(&mut self, v: i64) {
        self.zigzag(v);
    }
    pub fn string_raw(&mut self, s: &str) {
        self.binary_raw(s.as_bytes());
    }
}
