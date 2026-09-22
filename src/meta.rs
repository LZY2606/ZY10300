//! Parquet metadata structures interpreted from generic thrift values.
use crate::thrift::TVal;
use anyhow::{bail, Result};

pub fn phys_type_name(t: i32) -> &'static str {
    match t {
        0 => "BOOLEAN",
        1 => "INT32",
        2 => "INT64",
        3 => "INT96",
        4 => "FLOAT",
        5 => "DOUBLE",
        6 => "BYTE_ARRAY",
        7 => "FIXED_LEN_BYTE_ARRAY",
        _ => "UNKNOWN",
    }
}

pub fn encoding_name(e: i32) -> &'static str {
    match e {
        0 => "PLAIN",
        2 => "PLAIN_DICTIONARY",
        3 => "RLE",
        4 => "BIT_PACKED",
        5 => "DELTA_BINARY_PACKED",
        6 => "DELTA_LENGTH_BYTE_ARRAY",
        7 => "DELTA_BYTE_ARRAY",
        8 => "RLE_DICTIONARY",
        9 => "BYTE_STREAM_SPLIT",
        _ => "UNKNOWN",
    }
}

pub fn codec_name(c: i32) -> &'static str {
    match c {
        0 => "UNCOMPRESSED",
        1 => "SNAPPY",
        2 => "GZIP",
        3 => "LZO",
        4 => "BROTLI",
        5 => "LZ4",
        6 => "ZSTD",
        7 => "LZ4_RAW",
        _ => "UNKNOWN",
    }
}

pub fn page_type_name(t: i32) -> &'static str {
    match t {
        0 => "DATA_PAGE",
        1 => "INDEX_PAGE",
        2 => "DICTIONARY_PAGE",
        3 => "DATA_PAGE_V2",
        _ => "UNKNOWN",
    }
}

#[derive(Debug, Clone)]
pub struct SchemaElement {
    pub typ: Option<i32>,
    pub type_length: Option<i32>,
    pub repetition: i32, // 0 required, 1 optional, 2 repeated; root defaults to 0
    pub name: String,
    pub num_children: i32,
    pub converted_type: Option<i32>,
}

#[derive(Debug, Clone, Default)]
pub struct Statistics {
    pub min: Option<Vec<u8>>,
    pub max: Option<Vec<u8>>,
    pub null_count: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ColumnMetaData {
    pub typ: i32,
    pub encodings: Vec<i32>,
    pub path: Vec<String>,
    pub codec: i32,
    pub num_values: i64,
    pub total_uncompressed_size: i64,
    pub total_compressed_size: i64,
    pub data_page_offset: i64,
    pub dictionary_page_offset: Option<i64>,
    pub statistics: Option<Statistics>,
}

#[derive(Debug, Clone)]
pub struct ColumnChunk {
    pub meta: ColumnMetaData,
    pub column_index_offset: Option<i64>,
    pub column_index_length: Option<i32>,
    pub offset_index_offset: Option<i64>,
    pub offset_index_length: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct RowGroupMeta {
    pub columns: Vec<ColumnChunk>,
    pub total_byte_size: i64,
    pub num_rows: i64,
}

#[derive(Debug, Clone)]
pub struct FileMeta {
    pub version: i32,
    pub schema: Vec<SchemaElement>,
    pub num_rows: i64,
    pub row_groups: Vec<RowGroupMeta>,
    pub created_by: Option<String>,
}

fn get_i64(fields: &[(i16, TVal)], id: i16) -> Option<i64> {
    TVal::get(fields, id).and_then(|v| v.as_i64())
}

fn parse_statistics(fields: &[(i16, TVal)]) -> Statistics {
    // Prefer min_value/max_value (fields 5/6), fall back to min/max (2/1).
    let min = TVal::get(fields, 5)
        .and_then(|v| v.as_bin().map(|b| b.to_vec()))
        .or_else(|| TVal::get(fields, 2).and_then(|v| v.as_bin().map(|b| b.to_vec())));
    let max = TVal::get(fields, 6)
        .and_then(|v| v.as_bin().map(|b| b.to_vec()))
        .or_else(|| TVal::get(fields, 1).and_then(|v| v.as_bin().map(|b| b.to_vec())));
    Statistics {
        min,
        max,
        null_count: get_i64(fields, 3),
    }
}

fn parse_column_metadata(fields: &[(i16, TVal)]) -> Result<ColumnMetaData> {
    let typ = get_i64(fields, 1).unwrap_or(-1) as i32;
    let encodings = TVal::get(fields, 2)
        .and_then(|v| v.as_list())
        .map(|l| l.iter().filter_map(|e| e.as_i64().map(|x| x as i32)).collect())
        .unwrap_or_default();
    let path = TVal::get(fields, 3)
        .and_then(|v| v.as_list())
        .map(|l| l.iter().filter_map(|e| e.as_str()).collect())
        .unwrap_or_default();
    let statistics = TVal::get(fields, 12)
        .and_then(|v| v.as_struct())
        .map(parse_statistics);
    Ok(ColumnMetaData {
        typ,
        encodings,
        path,
        codec: get_i64(fields, 4).unwrap_or(0) as i32,
        num_values: get_i64(fields, 5).unwrap_or(0),
        total_uncompressed_size: get_i64(fields, 6).unwrap_or(0),
        total_compressed_size: get_i64(fields, 7).unwrap_or(0),
        data_page_offset: get_i64(fields, 9).unwrap_or(0),
        dictionary_page_offset: get_i64(fields, 11),
        statistics,
    })
}

fn parse_column_chunk(fields: &[(i16, TVal)]) -> Result<ColumnChunk> {
    let meta = TVal::get(fields, 3)
        .and_then(|v| v.as_struct())
        .map(parse_column_metadata)
        .transpose()?
        .unwrap_or_else(|| ColumnMetaData {
            typ: -1,
            encodings: vec![],
            path: vec![],
            codec: 0,
            num_values: 0,
            total_uncompressed_size: 0,
            total_compressed_size: 0,
            data_page_offset: 0,
            dictionary_page_offset: None,
            statistics: None,
        });
    Ok(ColumnChunk {
        meta,
        column_index_offset: get_i64(fields, 4),
        column_index_length: get_i64(fields, 5).map(|v| v as i32),
        offset_index_offset: get_i64(fields, 6),
        offset_index_length: get_i64(fields, 7).map(|v| v as i32),
    })
}

pub fn parse_file_metadata(fields: &[(i16, TVal)]) -> Result<FileMeta> {
    let schema = TVal::get(fields, 2)
        .and_then(|v| v.as_list())
        .map(|l| {
            l.iter()
                .filter_map(|e| e.as_struct())
                .map(|s| SchemaElement {
                    typ: get_i64(s, 1).map(|v| v as i32),
                    type_length: get_i64(s, 2).map(|v| v as i32),
                    repetition: get_i64(s, 3).unwrap_or(0) as i32,
                    name: TVal::get(s, 4).and_then(|v| v.as_str()).unwrap_or_default(),
                    num_children: get_i64(s, 5).unwrap_or(0) as i32,
                    converted_type: get_i64(s, 6).map(|v| v as i32),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let row_groups = TVal::get(fields, 4)
        .and_then(|v| v.as_list())
        .map(|l| {
            l.iter()
                .filter_map(|e| e.as_struct())
                .map(|rg| {
                    let columns = TVal::get(rg, 1)
                        .and_then(|v| v.as_list())
                        .map(|cl| {
                            cl.iter()
                                .filter_map(|c| c.as_struct())
                                .filter_map(|c| parse_column_chunk(c).ok())
                                .collect()
                        })
                        .unwrap_or_default();
                    RowGroupMeta {
                        columns,
                        total_byte_size: get_i64(rg, 2).unwrap_or(0),
                        num_rows: get_i64(rg, 3).unwrap_or(0),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if schema.is_empty() {
        bail!("parquet: file metadata has no schema");
    }
    Ok(FileMeta {
        version: get_i64(fields, 1).unwrap_or(1) as i32,
        schema,
        num_rows: get_i64(fields, 3).unwrap_or(0),
        row_groups,
        created_by: TVal::get(fields, 6).and_then(|v| v.as_str()),
    })
}

#[derive(Debug, Clone)]
pub struct DataPageV1 {
    pub num_values: i32,
    pub encoding: i32,
    pub def_encoding: i32,
    pub rep_encoding: i32,
}

#[derive(Debug, Clone)]
pub struct DataPageV2 {
    pub num_values: i32,
    pub num_nulls: i32,
    pub num_rows: i32,
    pub encoding: i32,
    pub def_levels_byte_length: i32,
    pub rep_levels_byte_length: i32,
    pub is_compressed: bool,
    pub statistics: Option<Statistics>,
}

#[derive(Debug, Clone)]
pub struct DictPageHeader {
    pub num_values: i32,
    pub encoding: i32,
}

#[derive(Debug, Clone)]
pub struct PageHeader {
    pub typ: i32,
    pub uncompressed_page_size: i32,
    pub compressed_page_size: i32,
    pub crc: Option<i32>,
    pub data_v1: Option<DataPageV1>,
    pub data_v2: Option<DataPageV2>,
    pub dict: Option<DictPageHeader>,
}

pub fn parse_page_header(fields: &[(i16, TVal)]) -> PageHeader {
    let data_v1 = TVal::get(fields, 5).and_then(|v| v.as_struct()).map(|s| DataPageV1 {
        num_values: get_i64(s, 1).unwrap_or(0) as i32,
        encoding: get_i64(s, 2).unwrap_or(0) as i32,
        def_encoding: get_i64(s, 3).unwrap_or(3) as i32,
        rep_encoding: get_i64(s, 4).unwrap_or(3) as i32,
    });
    let data_v2 = TVal::get(fields, 8).and_then(|v| v.as_struct()).map(|s| DataPageV2 {
        num_values: get_i64(s, 1).unwrap_or(0) as i32,
        num_nulls: get_i64(s, 2).unwrap_or(0) as i32,
        num_rows: get_i64(s, 3).unwrap_or(0) as i32,
        encoding: get_i64(s, 4).unwrap_or(0) as i32,
        def_levels_byte_length: get_i64(s, 5).unwrap_or(0) as i32,
        rep_levels_byte_length: get_i64(s, 6).unwrap_or(0) as i32,
        is_compressed: TVal::get(s, 7).and_then(|v| v.as_bool()).unwrap_or(true),
        statistics: TVal::get(s, 8).and_then(|v| v.as_struct()).map(parse_statistics),
    });
    let dict = TVal::get(fields, 7).and_then(|v| v.as_struct()).map(|s| DictPageHeader {
        num_values: get_i64(s, 1).unwrap_or(0) as i32,
        encoding: get_i64(s, 2).unwrap_or(0) as i32,
    });
    PageHeader {
        typ: get_i64(fields, 1).unwrap_or(0) as i32,
        uncompressed_page_size: get_i64(fields, 2).unwrap_or(0) as i32,
        compressed_page_size: get_i64(fields, 3).unwrap_or(0) as i32,
        crc: get_i64(fields, 4).map(|v| v as i32),
        data_v1,
        data_v2,
        dict,
    }
}

#[derive(Debug, Clone)]
pub struct PageLocation {
    pub offset: i64,
    pub compressed_page_size: i32,
    pub first_row_index: i64,
}

pub fn parse_offset_index(fields: &[(i16, TVal)]) -> Vec<PageLocation> {
    TVal::get(fields, 1)
        .and_then(|v| v.as_list())
        .map(|l| {
            l.iter()
                .filter_map(|e| e.as_struct())
                .map(|s| PageLocation {
                    offset: get_i64(s, 1).unwrap_or(0),
                    compressed_page_size: get_i64(s, 2).unwrap_or(0) as i32,
                    first_row_index: get_i64(s, 3).unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default)]
pub struct ColumnIndex {
    pub null_pages: Vec<bool>,
    pub min_values: Vec<Vec<u8>>,
    pub max_values: Vec<Vec<u8>>,
    pub null_counts: Vec<i64>,
}

pub fn parse_column_index(fields: &[(i16, TVal)]) -> ColumnIndex {
    let bools = |id: i16| {
        TVal::get(fields, id)
            .and_then(|v| v.as_list())
            .map(|l| l.iter().filter_map(|e| e.as_bool()).collect())
            .unwrap_or_default()
    };
    let bins = |id: i16| {
        TVal::get(fields, id)
            .and_then(|v| v.as_list())
            .map(|l| {
                l.iter()
                    .filter_map(|e| e.as_bin().map(|b| b.to_vec()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let i64s = |id: i16| {
        TVal::get(fields, id)
            .and_then(|v| v.as_list())
            .map(|l| l.iter().filter_map(|e| e.as_i64()).collect())
            .unwrap_or_default()
    };
    ColumnIndex {
        null_pages: bools(1),
        min_values: bins(2),
        max_values: bins(3),
        null_counts: i64s(5),
    }
}
