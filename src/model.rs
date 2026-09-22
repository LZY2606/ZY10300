//! Schema tree, logical value model, issues, lineage spans, and the
//! shredded-record assembler (definition/repetition levels -> logical rows).
use crate::meta::SchemaElement;
use anyhow::{bail, Result};
use serde_json::{json, Value as Json};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Issue {
    pub severity: String, // "error" | "warn" | "info"
    pub kind: String,
    pub message: String,
    pub column: Option<String>,
    pub page: Option<usize>,
}

impl Issue {
    pub fn error(kind: &str, message: impl Into<String>) -> Issue {
        Issue {
            severity: "error".into(),
            kind: kind.into(),
            message: message.into(),
            column: None,
            page: None,
        }
    }
    pub fn info(kind: &str, message: impl Into<String>) -> Issue {
        Issue {
            severity: "info".into(),
            kind: kind.into(),
            message: message.into(),
            column: None,
            page: None,
        }
    }
    pub fn for_column(mut self, col: &str) -> Issue {
        self.column = Some(col.to_string());
        self
    }
    pub fn for_page(mut self, page: usize) -> Issue {
        self.page = Some(page);
        self
    }
}

/// Byte-range lineage: which file bytes produced a logical value.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Span {
    pub kind: String, // "def_levels" | "rep_levels" | "values" | "dict_indices" | "dictionary"
    pub start: u64,   // absolute file offset, inclusive
    pub end: u64,     // exclusive
    pub page: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Prim {
    Bool(bool),
    I32(i32),
    I64(i64),
    I96([u8; 12]), // kept as raw 12 bytes; never reinterpreted as a timestamp
    F32(f32),
    F64(f64),
    Bytes(Vec<u8>),
    Fixed(Vec<u8>),
}

impl Prim {
    pub fn to_json(&self) -> Json {
        match self {
            Prim::Bool(b) => json!(b),
            Prim::I32(v) => json!(v),
            Prim::I64(v) => json!(v),
            Prim::I96(b) => json!({ "int96_hex": hex(b) }),
            Prim::F32(v) => json!(v),
            Prim::F64(v) => json!(v),
            Prim::Bytes(b) | Prim::Fixed(b) => match std::str::from_utf8(b) {
                Ok(s) => json!(s),
                Err(_) => json!({ "hex": hex(b) }),
            },
        }
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

#[derive(Debug, Clone)]
pub struct SchemaNode {
    pub name: String,
    pub repetition: i32, // 0 required, 1 optional, 2 repeated
    pub def_level: u16,
    pub rep_level: u16,
    pub leaf: Option<LeafInfo>,
    pub children: Vec<SchemaNode>,
    pub path: String, // dot-joined from root (excluding root)
}

#[derive(Debug, Clone)]
pub struct LeafInfo {
    pub typ: i32,
    pub type_length: i32,
}

impl SchemaNode {
    pub fn is_leaf(&self) -> bool {
        self.leaf.is_some()
    }
    pub fn leaves(&self) -> Vec<&SchemaNode> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }
    fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a SchemaNode>) {
        if self.is_leaf() {
            out.push(self);
        }
        for c in &self.children {
            c.collect_leaves(out);
        }
    }
    pub fn find_leaf(&self, path: &str) -> Option<&SchemaNode> {
        self.leaves().into_iter().find(|l| l.path == path)
    }
}

pub fn build_schema(elements: &[SchemaElement]) -> Result<SchemaNode> {
    if elements.is_empty() {
        bail!("empty schema");
    }
    let mut idx = 0usize;
    let root = build_node(elements, &mut idx, 0, 0, "", true)?;
    Ok(root)
}

fn build_node(
    elements: &[SchemaElement],
    idx: &mut usize,
    parent_def: u16,
    parent_rep: u16,
    parent_path: &str,
    is_root: bool,
) -> Result<SchemaNode> {
    if *idx >= elements.len() {
        bail!("schema element index out of range");
    }
    let el = &elements[*idx];
    *idx += 1;
    let repetition = if is_root { 0 } else { el.repetition };
    let def_level = parent_def + if repetition != 0 { 1 } else { 0 };
    let rep_level = parent_rep + if repetition == 2 { 1 } else { 0 };
    let path = if parent_path.is_empty() {
        el.name.clone()
    } else {
        format!("{}.{}", parent_path, el.name)
    };
    let mut node = SchemaNode {
        name: el.name.clone(),
        repetition,
        def_level,
        rep_level,
        leaf: None,
        children: Vec::new(),
        path: if is_root { String::new() } else { path },
    };
    for _ in 0..el.num_children.max(0) {
        let child = build_node(elements, idx, def_level, rep_level, &node.path, false)?;
        node.children.push(child);
    }
    if el.num_children == 0 {
        node.leaf = Some(LeafInfo {
            typ: el.typ.unwrap_or(-1),
            type_length: el.type_length.unwrap_or(0),
        });
    }
    Ok(node)
}

/// One shredded value: levels + optional primitive + byte lineage.
#[derive(Debug, Clone)]
pub struct Triple {
    pub rep: u16,
    pub def: u16,
    pub val: Option<Prim>,
    pub spans: Vec<Span>,
}

/// Logical (assembled) value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Prim(Prim),
    List(Vec<Value>),
    Struct(Vec<(String, Value)>),
}

impl Value {
    pub fn to_json(&self) -> Json {
        match self {
            Value::Null => Json::Null,
            Value::Prim(p) => p.to_json(),
            Value::List(items) => Json::Array(items.iter().map(|v| v.to_json()).collect()),
            Value::Struct(fields) => {
                let mut m = serde_json::Map::new();
                for (k, v) in fields {
                    m.insert(k.clone(), v.to_json());
                }
                Json::Object(m)
            }
        }
    }
}

/// Assemble `num_rows` logical records of the column rooted at `node`
/// (the path from `node` down to the leaf is the column's schema path).
/// `node` should be the file root; `triples` belong to one leaf column.
/// Returns per-row values plus lineage. Local issues are recorded and
/// decoding continues with partial data where possible.
pub fn assemble_column(
    root: &SchemaNode,
    leaf_path: &str,
    triples: &[Triple],
    num_rows: usize,
    issues: &mut Vec<Issue>,
) -> Vec<(Value, Vec<Span>)> {
    // Build the chain of nodes from root to the leaf.
    let mut chain: Vec<&SchemaNode> = Vec::new();
    if !find_chain(root, leaf_path, &mut chain) {
        issues.push(Issue::error("SchemaPath", format!("leaf {} not found", leaf_path)));
        return Vec::new();
    }
    let mut pos = 0usize;
    let mut rows = Vec::new();
    for row in 0..num_rows {
        if pos >= triples.len() {
            issues.push(
                Issue::error(
                    "RowReconstruction",
                    format!("ran out of values at row {} ({} rows assembled)", row, rows.len()),
                )
                .for_column(leaf_path),
            );
            break;
        }
        let mut spans = Vec::new();
        let v = assemble_field(&chain[1..], triples, &mut pos, &mut spans, issues, leaf_path);
        rows.push((v, spans));
    }
    if pos < triples.len() {
        issues.push(
            Issue::error(
                "RowReconstruction",
                format!(
                    "{} decoded values left over after {} rows (corrupt levels?)",
                    triples.len() - pos,
                    num_rows
                ),
            )
            .for_column(leaf_path),
        );
    }
    rows
}

fn find_chain<'a>(node: &'a SchemaNode, leaf_path: &str, chain: &mut Vec<&'a SchemaNode>) -> bool {
    chain.push(node);
    if node.path == leaf_path && node.is_leaf() {
        return true;
    }
    for c in &node.children {
        if find_chain(c, leaf_path, chain) {
            return true;
        }
    }
    chain.pop();
    false
}

fn assemble_field(
    chain: &[&SchemaNode],
    triples: &[Triple],
    pos: &mut usize,
    spans: &mut Vec<Span>,
    issues: &mut Vec<Issue>,
    leaf_path: &str,
) -> Value {
    let node = chain[0];
    match node.repetition {
        0 => assemble_content(chain, triples, pos, spans, issues, leaf_path),
        1 => {
            // optional: exactly one triple per parent instance
            let t = &triples[*pos];
            if t.def >= node.def_level {
                assemble_content(chain, triples, pos, spans, issues, leaf_path)
            } else {
                spans.extend(t.spans.clone());
                *pos += 1;
                Value::Null
            }
        }
        2 => {
            let mut items = Vec::new();
            loop {
                if *pos >= triples.len() {
                    if items.is_empty() {
                        issues.push(
                            Issue::error("RowReconstruction", "missing repeated field marker")
                                .for_column(leaf_path),
                        );
                    }
                    break;
                }
                let t = &triples[*pos];
                if !items.is_empty() && t.rep < node.rep_level {
                    break;
                }
                if t.def < node.def_level {
                    // empty marker (zero occurrences at this level)
                    spans.extend(t.spans.clone());
                    *pos += 1;
                    if items.is_empty() {
                        return Value::List(vec![]);
                    }
                    issues.push(
                        Issue::error("RowReconstruction", "empty marker mid-list")
                            .for_column(leaf_path),
                    );
                    continue;
                }
                items.push(assemble_content(chain, triples, pos, spans, issues, leaf_path));
            }
            Value::List(items)
        }
        _ => {
            issues.push(Issue::error("Schema", "bad repetition type").for_column(leaf_path));
            Value::Null
        }
    }
}

fn assemble_content(
    chain: &[&SchemaNode],
    triples: &[Triple],
    pos: &mut usize,
    spans: &mut Vec<Span>,
    issues: &mut Vec<Issue>,
    leaf_path: &str,
) -> Value {
    let node = chain[0];
    if node.is_leaf() {
        let t = &triples[*pos];
        spans.extend(t.spans.clone());
        *pos += 1;
        match &t.val {
            Some(p) => Value::Prim(p.clone()),
            None => {
                issues.push(
                    Issue::error(
                        "RowReconstruction",
                        format!("leaf {} reached with no value (def={})", leaf_path, t.def),
                    )
                    .for_column(leaf_path),
                );
                Value::Null
            }
        }
    } else {
        let mut fields = Vec::new();
        for child in &node.children {
            // descend one level: rebuild the sub-chain for this child
            let mut sub: Vec<&SchemaNode> = vec![child];
            // extend sub-chain from the remaining chain if it matches
            if chain.len() > 1 && std::ptr::eq(chain[1], child) {
                sub.extend_from_slice(&chain[2..]);
                fields.push((
                    child.name.clone(),
                    assemble_field(&sub, triples, pos, spans, issues, leaf_path),
                ));
            } else {
                // child not on the path to our leaf: skip (other columns handle it)
                continue;
            }
        }
        Value::Struct(fields)
    }
}
