//! RLE / bit-packed hybrid decoding (definition & repetition levels,
//! dictionary indices) with per-run byte lineage and local error capture.
use crate::model::{Issue, Span};

pub fn bit_width_for(max_level: u16) -> u32 {
    32 - (max_level as u32).leading_zeros()
}

fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut shift = 0u32;
    let mut out = 0u64;
    loop {
        if *pos >= data.len() {
            return None;
        }
        let b = data[*pos];
        *pos += 1;
        out |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(out);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

/// Decode a hybrid run-length / bit-packed stream. Returns (value, span)
/// pairs. Stops at `expected` values or end of data; records local issues
/// instead of failing hard so partially-decoded data survives.
pub fn decode_hybrid(
    data: &[u8],
    base_offset: u64,
    bit_width: u32,
    expected: usize,
    kind: &str,
    column: &str,
    page: usize,
    issues: &mut Vec<Issue>,
) -> Vec<(u16, Span)> {
    let mut out: Vec<(u16, Span)> = Vec::new();
    if bit_width > 32 {
        issues.push(
            Issue::error(
                "LevelDecode",
                format!("invalid bit width {} (> 32)", bit_width),
            )
            .for_column(column)
            .for_page(page),
        );
        return out;
    }
    let byte_width = ((bit_width + 7) / 8) as usize;
    let mut pos = 0usize;
    // Safety cap against corrupt run lengths exploding memory.
    let hard_cap = expected.saturating_mul(4).max(1024);
    while pos < data.len() && out.len() < expected && out.len() < hard_cap {
        let run_start = pos;
        let header = match read_varint(data, &mut pos) {
            Some(h) => h,
            None => {
                issues.push(
                    Issue::error("LevelDecode", "truncated run header")
                        .for_column(column)
                        .for_page(page),
                );
                break;
            }
        };
        if header & 1 == 1 {
            // bit-packed run: header>>1 groups of 8 values
            let groups = (header >> 1) as usize;
            let nbytes = groups * bit_width as usize;
            if pos + nbytes > data.len() {
                issues.push(
                    Issue::error(
                        "LevelDecode",
                        format!(
                            "bit-packed run of {} groups needs {} bytes, only {} left",
                            groups,
                            nbytes,
                            data.len() - pos
                        ),
                    )
                    .for_column(column)
                    .for_page(page),
                );
                // decode what we can from complete groups
                let avail_groups = (data.len() - pos) / bit_width.max(1) as usize;
                let avail_groups = avail_groups.min(groups);
                decode_bitpacked(
                    &data[pos..pos + avail_groups * bit_width as usize],
                    bit_width,
                    base_offset + run_start as u64,
                    (pos + avail_groups * bit_width as usize) - run_start,
                    kind,
                    &mut out,
                );
                break;
            }
            decode_bitpacked(
                &data[pos..pos + nbytes],
                bit_width,
                base_offset + run_start as u64,
                (pos + nbytes) - run_start,
                kind,
                &mut out,
            );
            pos += nbytes;
        } else {
            // RLE run
            let count = (header >> 1) as usize;
            if count > hard_cap {
                issues.push(
                    Issue::error(
                        "LevelDecode",
                        format!("implausible RLE run length {}", count),
                    )
                    .for_column(column)
                    .for_page(page),
                );
                break;
            }
            if pos + byte_width > data.len() {
                issues.push(
                    Issue::error("LevelDecode", "truncated RLE run value")
                        .for_column(column)
                        .for_page(page),
                );
                break;
            }
            let mut v: u64 = 0;
            for i in 0..byte_width {
                v |= (data[pos + i] as u64) << (8 * i);
            }
            let span = Span {
                kind: kind.to_string(),
                start: base_offset + run_start as u64,
                end: base_offset + (pos + byte_width) as u64,
                page: Some(page),
            };
            pos += byte_width;
            for _ in 0..count {
                if out.len() >= hard_cap {
                    break;
                }
                out.push((v as u16, span.clone()));
            }
        }
    }
    if out.len() < expected {
        issues.push(
            Issue::error(
                "LevelDecode",
                format!(
                    "{} stream truncated: decoded {} of {} expected values",
                    kind,
                    out.len(),
                    expected
                ),
            )
            .for_column(column)
            .for_page(page),
        );
    }
    out
}

fn decode_bitpacked(
    data: &[u8],
    bit_width: u32,
    base: u64,
    span_len: usize,
    kind: &str,
    out: &mut Vec<(u16, Span)>,
) {
    if bit_width == 0 {
        return;
    }
    let total_bits = data.len() * 8;
    let count = total_bits / bit_width as usize;
    let span = Span {
        kind: kind.to_string(),
        start: base,
        end: base + span_len as u64,
        page: None,
    };
    for i in 0..count {
        let bit = i * bit_width as usize;
        let byte = bit / 8;
        let shift = bit % 8;
        let mut v: u64 = 0;
        let mut got = 0;
        let mut b = byte;
        let mut s = shift;
        while got < bit_width as usize && b < data.len() {
            let take = (8 - s).min(bit_width as usize - got);
            let mask = ((1u16 << take) - 1) as u8;
            v |= (((data[b] >> s) & mask) as u64) << got;
            got += take;
            b += 1;
            s = 0;
        }
        out.push((v as u16, span.clone()));
    }
}

/// Decode a u32-index dictionary stream (first byte = bit width, then hybrid).
pub fn decode_dict_indices(
    data: &[u8],
    base_offset: u64,
    expected: usize,
    column: &str,
    page: usize,
    issues: &mut Vec<Issue>,
) -> Vec<(u32, Span)> {
    if data.is_empty() {
        issues.push(
            Issue::error("DictDecode", "empty dictionary index stream")
                .for_column(column)
                .for_page(page),
        );
        return Vec::new();
    }
    let bit_width = data[0] as u32;
    if bit_width > 32 {
        issues.push(
            Issue::error(
                "DictDecode",
                format!("invalid dictionary bit width {}", bit_width),
            )
            .for_column(column)
            .for_page(page),
        );
        return Vec::new();
    }
    let raw = decode_hybrid(
        &data[1..],
        base_offset + 1,
        bit_width,
        expected,
        "dict_indices",
        column,
        page,
        issues,
    );
    raw.into_iter().map(|(v, s)| (v as u32, s)).collect()
}
