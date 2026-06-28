//! Big-endian byte reader/writer matching Distant Horizons' Netty `ByteBuf` serialization.
//!
//! DH writes primitives in network (big-endian) byte order:
//! `writeInt` = 4 bytes BE, `writeLong` = 8 bytes BE, `writeBoolean` = 1 byte,
//! `writeShort`/`readUnsignedShort` = 2 bytes BE, strings = 2-byte length + UTF-8.

pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn write_bool(&mut self, value: bool) {
        self.buf.push(if value { 1 } else { 0 });
    }

    pub fn write_i32(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_i64(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }

    pub fn write_string(&mut self, value: &str) {
        let bytes = value.as_bytes();
        // DH writes a 2-byte (unsigned) length prefix; dimension keys are short.
        let len = u16::try_from(bytes.len()).unwrap_or_else(|_| {
            panic!("string too long for DH wire format: {} bytes", bytes.len())
        });
        self.buf.extend_from_slice(&len.to_be_bytes());
        self.buf.extend_from_slice(bytes);
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }
}

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.pos..self.pos + count)?;
        self.pos += count;
        Some(slice)
    }

    pub fn read_u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_i32(&mut self) -> Option<i32> {
        let bytes = self.take(4)?;
        Some(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_bool(&mut self) -> Option<bool> {
        Some(self.take(1)?[0] != 0)
    }

    pub fn read_string(&mut self) -> Option<String> {
        let length = self.read_u16()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).ok()
    }
}
