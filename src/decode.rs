//! PLAIN decoding for physical types + page decompression.
use crate::model::{Issue, Prim, Span};
use anyhow::{bail, Result};

pub fn decompress(codec: i32, data: &[u8], uncompressed_size: usize) -> Result<Vec<u8>> {
    match codec {
        0 => Ok(data.to_vec()),
        1 => Ok(snap::raw::Decoder::new().decompress_vec(data)?),
        other => bail!("unsupported codec {} (only UNCOMPRESSED and SNAPPY)", other),
    }
    .map(|v| {
        if uncompressed_size > 0 && v.len() != uncompressed_size {
            // caller may still use it; size check happens at a higher level
            v
        } else {
            v
        }
    })
}

/// Decode `count` PLAIN-encoded values of physical type `typ`.
/// Returns values with per-value byte spans (relative to `base_offset`).
/// Truncation produces a local issue and a partial result.
pub fn decode_plain(
    typ: i32,
    type_length: i32,
    data: &[u8],
    base_offset: u64,
    count: usize,
    column: &str,
    page: usize,
    issues: &mut Vec<Issue>,
) -> Vec<(Prim, Span)> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let span_of = |start: usize, end: usize| Span {
        kind: "values".to_string(),
        start: base_offset + start as u64,
        end: base_offset + end as u64,
        page: Some(page),
    };
    macro_rules! need {
        ($n:expr) => {
            if pos + $n > data.len() {
                issues.push(
                    Issue::error(
                        "ValueDecode",
                        format!(
                            "PLAIN data truncated at value {} of {} (need {} bytes, have {})",
                            out.len(),
                            count,
                            $n,
                            data.len() - pos
                        ),
                    )
                    .for_column(column)
                    .for_page(page),
                );
                return out;
            }
        };
    }
    for _ in 0..count {
        let start = pos;
        let prim = match typ {
            0 => {
                // BOOLEAN: bit-packed LSB-first; handled outside the loop below
                match decode_booleans(data, base_offset, count, page) {
                    Some(v) => return v,
                    None => {
                        issues.push(
                            Issue::error("ValueDecode", "boolean stream truncated")
                                .for_column(column)
                                .for_page(page),
                        );
                        return out;
                    }
                }
            }
            1 => {
                need!(4);
                pos += 4;
                Prim::I32(i32::from_le_bytes(data[start..pos].try_into().unwrap()))
            }
            2 => {
                need!(8);
                pos += 8;
                Prim::I64(i64::from_le_bytes(data[start..pos].try_into().unwrap()))
            }
            3 => {
                need!(12);
                pos += 12;
                let mut b = [0u8; 12];
                b.copy_from_slice(&data[start..pos]);
                Prim::I96(b)
            }
            4 => {
                need!(4);
                pos += 4;
                Prim::F32(f32::from_le_bytes(data[start..pos].try_into().unwrap()))
            }
            5 => {
                need!(8);
                pos += 8;
                Prim::F64(f64::from_le_bytes(data[start..pos].try_into().unwrap()))
            }
            6 => {
                need!(4);
                let n = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4;
                need!(n);
                pos += n;
                Prim::Bytes(data[pos - n..pos].to_vec())
            }
            7 => {
                let n = type_length.max(0) as usize;
                need!(n);
                pos += n;
                Prim::Fixed(data[pos - n..pos].to_vec())
            }
            _ => {
                issues.push(
                    Issue::error("ValueDecode", format!("unknown physical type {}", typ))
                        .for_column(column)
                        .for_page(page),
                );
                return out;
            }
        };
        out.push((prim, span_of(start, pos)));
    }
    out
}

fn decode_booleans(
    data: &[u8],
    base_offset: u64,
    count: usize,
    page: usize,
) -> Option<Vec<(Prim, Span)>> {
    let nbytes = count.div_ceil(8);
    if data.len() < nbytes {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let b = (data[i / 8] >> (i % 8)) & 1 == 1;
        out.push((
            Prim::Bool(b),
            Span {
                kind: "values".into(),
                start: base_offset + (i / 8) as u64,
                end: base_offset + (i / 8 + 1) as u64,
                page: Some(page),
            },
        ));
    }
    Some(out)
}
