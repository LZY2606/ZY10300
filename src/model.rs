//! Shared data model: parquet metadata structs, values, errors.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalType {
    Boolean,
    Int32,
    Int64,
    Int96,
    Float,
    Double,
    ByteArray,
    FixedLenByteArray,
}

impl PhysicalType {
    pub fn from_i32(v: i32) -> Result<Self, String> {
        Ok(match v {
            0 => Self::Boolean,
            1 => Self::Int32,
            2 => Self::Int64,
            3 => Self::Int96,
            4 => Self::Float,
            5 => Self::Double,
            6 => Self::ByteArray,
            7 => Self::FixedLenByteArray,
            other => return Err(format!("未知物理类型 {other}")),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::Boolean => "BOOLEAN",
            Self::Int32 => "INT32",
            Self::Int64 => "INT64",
            Self::Int96 => "INT96",
            Self::Float => "FLOAT",
            Self::Double => "DOUBLE",
            Self::ByteArray => "BYTE_ARRAY",
            Self::FixedLenByteArray => "FIXED_LEN_BYTE_ARRAY",
        }
    }
}

pub fn encoding_name(e: i32) -> String {
    match e {
        0 => "PLAIN".into(),
        1 => "GROUP_VAR_INT".into(),
        2 => "PLAIN_DICTIONARY".into(),
        3 => "RLE".into(),
        4 => "BIT_PACKED".into(),
        5 => "DELTA_BINARY_PACKED".into(),
        6 => "DELTA_LENGTH_BYTE_ARRAY".into(),
        7 => "DELTA_BYTE_ARRAY".into(),
        8 => "RLE_DICTIONARY".into(),
        9 => "BYTE_STREAM_SPLIT".into(),
        other => format!("UNKNOWN({other})"),
    }
}

pub fn codec_name(c: i32) -> String {
    match c {
        0 => "UNCOMPRESSED".into(),
        1 => "SNAPPY".into(),
        2 => "GZIP".into(),
        3 => "LZO".into(),
        4 => "BROTLI".into(),
        5 => "LZ4".into(),
        6 => "ZSTD".into(),
        7 => "LZ4_RAW".into(),
        other => format!("UNKNOWN({other})"),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Statistics {
    pub min: Option<Vec<u8>>,
    pub max: Option<Vec<u8>>,
    pub null_count: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct SchemaElement {
    pub type_: Option<i32>,
    pub type_length: Option<i32>,
    pub repetition: Option<i32>, // 0 required, 1 optional, 2 repeated
    pub name: String,
    pub num_children: i32,
    pub converted_type: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct ColumnMeta {
    pub physical: i32,
    pub encodings: Vec<i32>,
    pub path: Vec<String>,
    pub codec: i32,
    pub num_values: i64,
    pub total_uncompressed: i64,
    pub total_compressed: i64,
    pub data_page_offset: i64,
    pub dictionary_page_offset: Option<i64>,
    pub statistics: Option<Statistics>,
    pub file_offset: i64,
    pub offset_index_offset: Option<i64>,
    pub offset_index_length: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct RowGroupMeta {
    pub columns: Vec<ColumnMeta>,
    pub num_rows: i64,
    pub total_byte_size: i64,
}

#[derive(Clone, Debug)]
pub struct FileMeta {
    pub version: i32,
    pub schema: Vec<SchemaElement>,
    pub num_rows: i64,
    pub row_groups: Vec<RowGroupMeta>,
    pub created_by: Option<String>,
    pub footer_len: usize,
}

#[derive(Clone, Debug, Default)]
pub struct DataPageHeader {
    pub num_values: i32,
    pub encoding: i32,
    pub def_encoding: i32,
    pub rep_encoding: i32,
    pub statistics: Option<Statistics>,
}

#[derive(Clone, Debug, Default)]
pub struct DataPageHeaderV2 {
    pub num_values: i32,
    pub num_nulls: i32,
    pub num_rows: i32,
    pub encoding: i32,
    pub def_levels_byte_length: i32,
    pub rep_levels_byte_length: i32,
    pub is_compressed: bool,
    pub statistics: Option<Statistics>,
}

#[derive(Clone, Debug, Default)]
pub struct DictPageHeader {
    pub num_values: i32,
    pub encoding: i32,
}

#[derive(Clone, Debug, Default)]
pub struct PageHeader {
    pub page_type: i32, // 0 data v1, 1 index, 2 dict, 3 data v2
    pub uncompressed_size: i32,
    pub compressed_size: i32,
    pub crc: Option<i32>,
    pub v1: Option<DataPageHeader>,
    pub v2: Option<DataPageHeaderV2>,
    pub dict: Option<DictPageHeader>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    I32(i32),
    I64(i64),
    I96([u8; 12]),
    F32(f32),
    F64(f64),
    Bytes(Vec<u8>),
}

impl Value {
    /// Human readable rendering. INT96 stays raw: julian day + nanos, no timezone.
    pub fn display(&self) -> String {
        match self {
            Value::Bool(b) => b.to_string(),
            Value::I32(v) => v.to_string(),
            Value::I64(v) => v.to_string(),
            Value::F32(v) => v.to_string(),
            Value::F64(v) => v.to_string(),
            Value::Bytes(b) => match std::str::from_utf8(b) {
                Ok(s) => format!("\"{s}\""),
                Err(_) => format!("0x{}", hex(b)),
            },
            Value::I96(raw) => {
                let nanos = u64::from_le_bytes(raw[0..8].try_into().unwrap());
                let day = u32::from_le_bytes(raw[8..12].try_into().unwrap());
                format!("INT96(julian_day={day}, nanos_in_day={nanos}, raw=0x{})", hex(raw))
            }
        }
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Clone, Debug)]
pub struct DissectError {
    pub kind: String,
    pub message: String,
    pub page: Option<usize>,
}

impl DissectError {
    pub fn new(kind: &str, message: impl Into<String>, page: Option<usize>) -> Self {
        DissectError { kind: kind.to_string(), message: message.into(), page }
    }
}

/// One decoded (rep, def, value) triple with page-level lineage.
#[derive(Clone, Debug)]
pub struct Triple {
    pub rep: u16,
    pub def: u16,
    pub value: Option<Value>,
    pub page: usize,
    /// File byte range of the value inside the page value stream, when byte-addressable.
    pub value_span: Option<(u64, u64)>,
}
