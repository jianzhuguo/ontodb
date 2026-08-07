//! Binary row format for fast filter evaluation without full JSON deserialization.
//!
//! Format layout:
//! ```text
//! [num_fields: u16 LE]
//! [field_name_lens: u16 LE × num_fields]    // name byte lengths
//! [field_type_tags: u8 × num_fields]         // 0=Null,1=Bool,2=Int,3=Float,4=Str,5=Obj,6=Arr
//! [field_name_bytes: variable]               // all names concatenated
//! [field_value_bytes: variable]              // all values concatenated (type-dependent encoding)
//! ```
//!
//! Value encoding:
//! - Null: 0 bytes
//! - Bool: 1 byte (0=false, 1=true)
//! - Int: 8 bytes big-endian i64
//! - Float: 8 bytes big-endian f64 (IEEE 754)
//! - String: [len: u32 LE] [utf8 bytes]
//! - Object: [len: u32 LE] [nested binary row bytes]
//! - Array: [len: u32 LE] [element_count: u32 LE] [elements...]

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::convert::TryInto;
use std::sync::OnceLock;

// Type tags
pub const TAG_NULL: u8 = 0;
pub const TAG_BOOL: u8 = 1;
pub const TAG_INT: u8 = 2;
pub const TAG_FLOAT: u8 = 3;
pub const TAG_STRING: u8 = 4;
pub const TAG_OBJECT: u8 = 5;
pub const TAG_ARRAY: u8 = 6;

/// A row stored in compact binary format. Wraps raw bytes with accessor methods.
pub struct BinaryRow<'a> {
    data: &'a [u8],
    num_fields: u16,
    /// Per-field metadata.
    fields: Vec<FieldMeta>,
    /// O(1) field name → index lookup, built lazily on first use.
    name_index: OnceLock<HashMap<&'a str, usize>>,
}

struct FieldMeta {
    name_len: u16,
    type_tag: u8,
    name_offset: usize,
    value_offset: usize,
    /// Pre-computed total value size in bytes (including length prefix for var-len types).
    value_size: usize,
}

impl<'a> BinaryRow<'a> {
    /// Parse a binary row from raw bytes. Returns None if the data is malformed.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < 2 {
            return None;
        }
        let num_fields = u16::from_le_bytes([data[0], data[1]]) as usize;
        let header_end = 2 + num_fields * 2 + num_fields; // name_lens + type_tags
        if data.len() < header_end {
            return None;
        }

        // Compute total name bytes inline (no temp Vec)
        let name_lens_base = 2usize;
        let tags_start = 2 + num_fields * 2;
        let names_start = tags_start + num_fields;

        let total_name_bytes: usize = (0..num_fields)
            .map(|i| {
                let off = name_lens_base + i * 2;
                u16::from_le_bytes([data[off], data[off + 1]]) as usize
            })
            .sum();

        // Parse type tags
        let type_tags: &[u8] = &data[tags_start..tags_start + num_fields];

        // Build field metadata with pre-computed value_size
        let mut name_offset = names_start;
        let mut value_offset = names_start + total_name_bytes;
        let mut fields = Vec::with_capacity(num_fields);

        for i in 0..num_fields {
            let off = name_lens_base + i * 2;
            let name_len = u16::from_le_bytes([data[off], data[off + 1]]) as usize;
            let type_tag = type_tags[i];

            // Validate value can be read and compute size
            let value_size = match type_tag {
                TAG_NULL => 0,
                TAG_BOOL => 1,
                TAG_INT | TAG_FLOAT => 8,
                TAG_STRING | TAG_OBJECT | TAG_ARRAY => {
                    if value_offset + 4 > data.len() {
                        return None;
                    }
                    let len = u32::from_le_bytes(
                        data[value_offset..value_offset + 4].try_into().ok()?,
                    ) as usize;
                    4 + len
                }
                _ => return None,
            };

            if value_offset + value_size > data.len() {
                return None;
            }

            fields.push(FieldMeta {
                name_len: name_len as u16,
                type_tag,
                name_offset,
                value_offset,
                value_size,
            });

            name_offset += name_len;
            value_offset += value_size;
        }

        // name_index is built lazily on first find_field() call
        Some(BinaryRow { data, num_fields: num_fields as u16, fields, name_index: OnceLock::new() })
    }

    /// Number of fields in this row.
    pub fn len(&self) -> usize {
        self.num_fields as usize
    }

    /// Get the field name for a given index.
    pub fn field_name(&self, idx: usize) -> &str {
        let fm = &self.fields[idx];
        let start = fm.name_offset;
        let end = start + fm.name_len as usize;
        std::str::from_utf8(&self.data[start..end]).unwrap_or("")
    }

    /// Get the type tag for a field by index.
    pub fn field_type(&self, idx: usize) -> u8 {
        self.fields[idx].type_tag
    }

    /// Get the raw value bytes and type tag for a field by index.
    pub fn field_value_raw(&self, idx: usize) -> (u8, &[u8]) {
        let fm = &self.fields[idx];
        (fm.type_tag, &self.data[fm.value_offset..fm.value_offset + fm.value_size])
    }

    /// Find a field by name. Returns its index, or None. O(1) via lazily-built HashMap.
    pub fn find_field(&self, name: &str) -> Option<usize> {
        let index = self.name_index.get_or_init(|| {
            let mut map = HashMap::with_capacity(self.num_fields as usize);
            for (i, fm) in self.fields.iter().enumerate() {
                let start = fm.name_offset;
                let end = start + fm.name_len as usize;
                if let Ok(n) = std::str::from_utf8(&self.data[start..end]) {
                    map.insert(n, i);
                }
            }
            map
        });
        index.get(name).copied()
    }

    /// Get the `__class__` field value as a string, if present.
    pub fn class_value(&self) -> Option<&str> {
        let idx = self.find_field("__class__")?;
        let (tag, raw) = self.field_value_raw(idx);
        if tag == TAG_STRING {
            parse_string_value(raw)
        } else {
            None
        }
    }

    /// Check if the `__class__` field matches any class in the hierarchy set.
    pub fn class_in_hierarchy(&self, hierarchy: &std::collections::HashSet<String>) -> bool {
        self.class_value().map_or(false, |c| hierarchy.contains(c))
    }

    /// Convert the binary row to a `Map<String, Value>`.
    pub fn to_map(&self) -> Option<Map<String, Value>> {
        let mut map = Map::with_capacity(self.num_fields as usize);
        for i in 0..self.num_fields as usize {
            let name = self.field_name(i).to_string();
            let (tag, raw) = self.field_value_raw(i);
            let value = binary_to_serde_value(tag, raw)?;
            map.insert(name, value);
        }
        Some(map)
    }

    /// Convert to a `Map` with only the requested columns (plus `__class__` if present).
    /// Falls back to full `to_map()` when `columns` is empty (SELECT *).
    /// This avoids converting all fields when only a subset is needed.
    pub fn to_map_projected(&self, columns: &[String]) -> Option<Map<String, Value>> {
        if columns.is_empty() {
            return self.to_map();
        }
        let mut map = Map::with_capacity(columns.len() + 1);
        // Always include __class__ for hierarchy checks downstream
        if let Some(cls) = self.class_value() {
            map.insert("__class__".to_string(), Value::String(cls.to_string()));
        }
        for col in columns {
            if col == "__class__" || col.starts_with("__") {
                continue;
            }
            if let Some(val) = self.get_value(col) {
                map.insert(col.clone(), val);
            }
        }
        Some(map)
    }

    /// Get a field value as a serde_json::Value by name.
    pub fn get_value(&self, name: &str) -> Option<Value> {
        let idx = self.find_field(name)?;
        let (tag, raw) = self.field_value_raw(idx);
        binary_to_serde_value(tag, raw)
    }

    /// Get field as f64 (Int or Float). Returns None if not numeric.
    pub fn get_f64(&self, name: &str) -> Option<f64> {
        let idx = self.find_field(name)?;
        let (tag, raw) = self.field_value_raw(idx);
        match tag {
            TAG_INT => {
                let arr: [u8; 8] = raw.try_into().ok()?;
                Some(i64::from_be_bytes(arr) as f64)
            }
            TAG_FLOAT => {
                let arr: [u8; 8] = raw.try_into().ok()?;
                Some(f64::from_be_bytes(arr))
            }
            _ => None,
        }
    }

    /// Get field as i64. Returns None if not an integer.
    pub fn get_i64(&self, name: &str) -> Option<i64> {
        let idx = self.find_field(name)?;
        let (tag, raw) = self.field_value_raw(idx);
        if tag == TAG_INT {
            let arr: [u8; 8] = raw.try_into().ok()?;
            Some(i64::from_be_bytes(arr))
        } else {
            None
        }
    }

    /// Get field as string slice. Returns None if not a string.
    pub fn get_str(&self, name: &str) -> Option<&str> {
        let idx = self.find_field(name)?;
        let (tag, raw) = self.field_value_raw(idx);
        if tag == TAG_STRING {
            parse_string_value(raw)
        } else {
            None
        }
    }

    /// Get field as bool. Returns None if not a boolean.
    pub fn get_bool(&self, name: &str) -> Option<bool> {
        let idx = self.find_field(name)?;
        let (tag, raw) = self.field_value_raw(idx);
        if tag == TAG_BOOL {
            Some(raw.first().map_or(false, |b| *b != 0))
        } else {
            None
        }
    }

    /// Check if a field is null (or missing).
    pub fn is_null(&self, name: &str) -> bool {
        match self.find_field(name) {
            None => true,
            Some(idx) => self.fields[idx].type_tag == TAG_NULL,
        }
    }

    /// Iterate over all fields as (name, tag, raw_value) triples.
    pub fn iter_fields(&self) -> impl Iterator<Item = (&str, u8, &[u8])> + use<'_, 'a> {
        (0..self.num_fields as usize).map(move |i| {
            let name = self.field_name(i);
            let (tag, raw) = self.field_value_raw(i);
            (name, tag, raw)
        })
    }
}

/// Convert binary row bytes directly to a `Map<String, Value>` in a single pass.
/// Skips building the `BinaryRow` struct entirely — no `Vec<FieldMeta>`, no `HashMap`.
/// This is the fastest path when you only need the `Map` result.
pub fn binary_bytes_to_map(data: &[u8]) -> Option<Map<String, Value>> {
    if data.len() < 2 {
        return None;
    }
    let num_fields = u16::from_le_bytes([data[0], data[1]]) as usize;
    let header_end = 2 + num_fields * 2 + num_fields;
    if data.len() < header_end {
        return None;
    }

    let name_lens_base = 2usize;
    let tags_start = 2 + num_fields * 2;
    let names_start = tags_start + num_fields;

    // Compute total name bytes
    let total_name_bytes: usize = (0..num_fields)
        .map(|i| {
            let off = name_lens_base + i * 2;
            u16::from_le_bytes([data[off], data[off + 1]]) as usize
        })
        .sum();

    let type_tags: &[u8] = &data[tags_start..tags_start + num_fields];
    let mut name_offset = names_start;
    let mut value_offset = names_start + total_name_bytes;
    let mut map = Map::with_capacity(num_fields);

    for i in 0..num_fields {
        let off = name_lens_base + i * 2;
        let name_len = u16::from_le_bytes([data[off], data[off + 1]]) as usize;
        let type_tag = type_tags[i];

        // Extract field name
        let name_end = name_offset + name_len;
        if name_end > data.len() {
            return None;
        }
        let name = std::str::from_utf8(&data[name_offset..name_end]).ok()?;

        // Parse value inline
        let value = match type_tag {
            TAG_NULL => Value::Null,
            TAG_BOOL => {
                if value_offset >= data.len() { return None; }
                Value::Bool(data[value_offset] != 0)
            }
            TAG_INT => {
                if value_offset + 8 > data.len() { return None; }
                let arr: [u8; 8] = data[value_offset..value_offset + 8].try_into().ok()?;
                Value::Number(serde_json::Number::from(i64::from_be_bytes(arr)))
            }
            TAG_FLOAT => {
                if value_offset + 8 > data.len() { return None; }
                let arr: [u8; 8] = data[value_offset..value_offset + 8].try_into().ok()?;
                let f = f64::from_be_bytes(arr);
                match serde_json::Number::from_f64(f) {
                    Some(n) => Value::Number(n),
                    None => return None,
                }
            }
            TAG_STRING => {
                if value_offset + 4 > data.len() { return None; }
                let slen = u32::from_le_bytes(
                    data[value_offset..value_offset + 4].try_into().ok()?,
                ) as usize;
                if value_offset + 4 + slen > data.len() { return None; }
                let s = std::str::from_utf8(&data[value_offset + 4..value_offset + 4 + slen]).ok()?;
                Value::String(s.to_string())
            }
            TAG_OBJECT => {
                if value_offset + 4 > data.len() { return None; }
                let olen = u32::from_le_bytes(
                    data[value_offset..value_offset + 4].try_into().ok()?,
                ) as usize;
                if value_offset + 4 + olen > data.len() { return None; }
                let inner = &data[value_offset + 4..value_offset + 4 + olen];
                match binary_bytes_to_map(inner) {
                    Some(m) => Value::Object(m),
                    None => return None,
                }
            }
            TAG_ARRAY => {
                if value_offset + 8 > data.len() { return None; }
                let arr_data = &data[value_offset..];
                binary_to_serde_value(TAG_ARRAY, arr_data)?
            }
            _ => return None,
        };

        map.insert(name.to_string(), value);

        // Advance offsets
        name_offset = name_end;
        let value_size = match type_tag {
            TAG_NULL => 0,
            TAG_BOOL => 1,
            TAG_INT | TAG_FLOAT => 8,
            TAG_STRING | TAG_OBJECT | TAG_ARRAY => {
                if value_offset + 4 > data.len() { return None; }
                4 + u32::from_le_bytes(
                    data[value_offset..value_offset + 4].try_into().ok()?,
                ) as usize
            }
            _ => return None,
        };
        value_offset += value_size;
    }

    Some(map)
}

// ─── Conversion: JSON Map → Binary ───────────────────────────────────────

/// Convert a `Map<String, Value>` to binary row bytes.
pub fn map_to_binary(map: &Map<String, Value>) -> Vec<u8> {
    let num_fields = map.len();
    let mut out = Vec::with_capacity(64 + num_fields * 20);

    // Header: num_fields
    out.extend_from_slice(&(num_fields as u16).to_le_bytes());

    // Collect field info
    let mut name_bytes = Vec::with_capacity(num_fields * 16);
    let mut value_bytes = Vec::with_capacity(num_fields * 32);
    let mut name_lens = Vec::with_capacity(num_fields);
    let mut type_tags = Vec::with_capacity(num_fields);

    for (key, val) in map {
        let name = key.as_bytes();
        name_lens.push(name.len() as u16);
        name_bytes.extend_from_slice(name);

        let (tag, vbytes) = serde_value_to_binary(val);
        type_tags.push(tag);
        value_bytes.extend_from_slice(&vbytes);
    }

    // Write name lengths
    for len in &name_lens {
        out.extend_from_slice(&len.to_le_bytes());
    }
    // Write type tags
    out.extend_from_slice(&type_tags);
    // Write name bytes
    out.extend_from_slice(&name_bytes);
    // Write value bytes
    out.extend_from_slice(&value_bytes);

    out
}

/// Serialize a serde_json::Value to binary format. Returns (tag, bytes).
fn serde_value_to_binary(val: &Value) -> (u8, Vec<u8>) {
    match val {
        Value::Null => (TAG_NULL, vec![]),
        Value::Bool(b) => (TAG_BOOL, vec![if *b { 1 } else { 0 }]),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                (TAG_INT, i.to_be_bytes().to_vec())
            } else if let Some(f) = n.as_f64() {
                (TAG_FLOAT, f.to_be_bytes().to_vec())
            } else {
                let s = n.to_string();
                let mut bytes = Vec::with_capacity(4 + s.len());
                bytes.extend_from_slice(&(s.len() as u32).to_le_bytes());
                bytes.extend_from_slice(s.as_bytes());
                (TAG_STRING, bytes)
            }
        }
        Value::String(s) => {
            let b = s.as_bytes();
            let mut bytes = Vec::with_capacity(4 + b.len());
            bytes.extend_from_slice(&(b.len() as u32).to_le_bytes());
            bytes.extend_from_slice(b);
            (TAG_STRING, bytes)
        }
        Value::Array(arr) => {
            let mut bytes = Vec::with_capacity(128);
            bytes.extend_from_slice(&[0u8; 4]); // total length placeholder
            bytes.extend_from_slice(&(arr.len() as u32).to_le_bytes());
            for elem in arr {
                let (tag, vbytes) = serde_value_to_binary(elem);
                bytes.push(tag);
                bytes.extend_from_slice(&(vbytes.len() as u32).to_le_bytes());
                bytes.extend_from_slice(&vbytes);
            }
            let total_len = (bytes.len() - 4) as u32;
            bytes[0..4].copy_from_slice(&total_len.to_le_bytes());
            (TAG_ARRAY, bytes)
        }
        Value::Object(obj) => {
            let nested = map_to_binary(obj);
            let mut bytes = Vec::with_capacity(4 + nested.len());
            bytes.extend_from_slice(&(nested.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&nested);
            (TAG_OBJECT, bytes)
        }
    }
}

/// Parse a serde_json::Value from binary format.
pub fn binary_to_serde_value(tag: u8, raw: &[u8]) -> Option<Value> {
    match tag {
        TAG_NULL => Some(Value::Null),
        TAG_BOOL => Some(Value::Bool(raw.first().map_or(false, |b| *b != 0))),
        TAG_INT => {
            let arr: [u8; 8] = raw.get(..8)?.try_into().ok()?;
            Some(Value::Number(serde_json::Number::from(i64::from_be_bytes(arr))))
        }
        TAG_FLOAT => {
            let arr: [u8; 8] = raw.get(..8)?.try_into().ok()?;
            let f = f64::from_be_bytes(arr);
            serde_json::Number::from_f64(f).map(Value::Number)
        }
        TAG_STRING => {
            let s = parse_string_value(raw)?;
            Some(Value::String(s.to_string()))
        }
        TAG_OBJECT => {
            let inner_bytes = parse_length_prefixed(raw)?;
            binary_bytes_to_map(inner_bytes).map(Value::Object)
        }
        TAG_ARRAY => {
            if raw.len() < 8 {
                return None;
            }
            let count = u32::from_le_bytes(raw[4..8].try_into().ok()?) as usize;
            let mut arr = Vec::with_capacity(count);
            let mut pos = 8;
            for _ in 0..count {
                if pos >= raw.len() {
                    return None;
                }
                let elem_tag = raw[pos];
                pos += 1;
                if pos + 4 > raw.len() {
                    return None;
                }
                let elem_len = u32::from_le_bytes(raw[pos..pos + 4].try_into().ok()?) as usize;
                pos += 4;
                if pos + elem_len > raw.len() {
                    return None;
                }
                let elem_val = binary_to_serde_value(elem_tag, &raw[pos..pos + elem_len])?;
                arr.push(elem_val);
                pos += elem_len;
            }
            Some(Value::Array(arr))
        }
        _ => None,
    }
}

/// Parse a length-prefixed string value.
pub fn parse_string_value(raw: &[u8]) -> Option<&str> {
    if raw.len() < 4 {
        return None;
    }
    let len = u32::from_le_bytes(raw[0..4].try_into().ok()?) as usize;
    if raw.len() < 4 + len {
        return None;
    }
    std::str::from_utf8(&raw[4..4 + len]).ok()
}

/// Parse a length-prefixed nested binary blob.
fn parse_length_prefixed(raw: &[u8]) -> Option<&[u8]> {
    if raw.len() < 4 {
        return None;
    }
    let len = u32::from_le_bytes(raw[0..4].try_into().ok()?) as usize;
    if raw.len() < 4 + len {
        return None;
    }
    Some(&raw[4..4 + len])
}

/// Check equality between a binary field value and a serde_json::Value.
/// Used by filter evaluation in onto-query.
pub fn binary_value_eq_serde(tag: u8, raw: &[u8], lit: &Value) -> bool {
    match (tag, lit) {
        (TAG_NULL, Value::Null) => true,
        (TAG_NULL, _) => false,
        (_, Value::Null) => false,
        (TAG_BOOL, Value::Bool(b)) => raw.first().map_or(false, |v| (*v != 0) == *b),
        (TAG_INT, Value::Number(n)) => {
            if let Ok(arr) = raw.try_into() as Result<[u8; 8], _> {
                let v = i64::from_be_bytes(arr);
                n.as_i64().map_or(false, |ni| v == ni)
                    || n.as_f64().map_or(false, |nf| v as f64 == nf)
            } else {
                false
            }
        }
        (TAG_FLOAT, Value::Number(n)) => {
            if let Ok(arr) = raw.try_into() as Result<[u8; 8], _> {
                let v = f64::from_be_bytes(arr);
                n.as_f64().map_or(false, |nf| v == nf)
                    || n.as_i64().map_or(false, |ni| v == ni as f64)
            } else {
                false
            }
        }
        (TAG_STRING, Value::String(s)) => parse_string_value(raw).map_or(false, |v| v == s.as_str()),
        _ => false,
    }
}

/// Compare a binary field value against a serde_json::Value.
/// Returns Some(Ordering) or None if incomparable.
pub fn binary_value_ord_serde(tag: u8, raw: &[u8], lit: &Value) -> Option<std::cmp::Ordering> {
    match (tag, lit) {
        (TAG_INT, Value::Number(n)) => {
            let arr: [u8; 8] = raw.try_into().ok()?;
            let v = i64::from_be_bytes(arr);
            if let Some(ni) = n.as_i64() {
                Some(v.cmp(&ni))
            } else if let Some(nf) = n.as_f64() {
                (v as f64).partial_cmp(&nf)
            } else {
                None
            }
        }
        (TAG_FLOAT, Value::Number(n)) => {
            let arr: [u8; 8] = raw.try_into().ok()?;
            let v = f64::from_be_bytes(arr);
            if let Some(nf) = n.as_f64() {
                v.partial_cmp(&nf)
            } else if let Some(ni) = n.as_i64() {
                v.partial_cmp(&(ni as f64))
            } else {
                None
            }
        }
        (TAG_STRING, Value::String(s)) => {
            parse_string_value(raw).map(|v| v.cmp(s.as_str()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_simple() {
        let mut map = Map::new();
        map.insert("name".into(), Value::String("Alice".into()));
        map.insert("age".into(), Value::Number(30.into()));
        map.insert("active".into(), Value::Bool(true));
        map.insert("score".into(), serde_json::json!(95.5));
        map.insert("notes".into(), Value::Null);

        let binary = map_to_binary(&map);
        let row = BinaryRow::parse(&binary).unwrap();

        assert_eq!(row.len(), 5);
        assert_eq!(row.class_value(), None);
        assert!(row.find_field("name").is_some());
        assert_eq!(row.get_value("name"), Some(Value::String("Alice".into())));
        assert_eq!(row.get_value("age"), Some(Value::Number(30.into())));
        assert_eq!(row.get_value("active"), Some(Value::Bool(true)));
        assert!(row.get_value("score").is_some());
        assert_eq!(row.get_value("notes"), Some(Value::Null));

        let back = row.to_map().unwrap();
        assert_eq!(back, map);
    }

    #[test]
    fn typed_accessors() {
        let mut map = Map::new();
        map.insert("price".into(), Value::Number(5000.into()));
        map.insert("name".into(), Value::String("Widget".into()));
        map.insert("ok".into(), Value::Bool(true));
        map.insert("empty".into(), Value::Null);
        let binary = map_to_binary(&map);
        let row = BinaryRow::parse(&binary).unwrap();

        assert_eq!(row.get_i64("price"), Some(5000));
        assert_eq!(row.get_f64("price"), Some(5000.0));
        assert_eq!(row.get_str("name"), Some("Widget"));
        assert_eq!(row.get_bool("ok"), Some(true));
        assert!(row.is_null("empty"));
        assert!(row.is_null("missing"));
    }

    #[test]
    fn roundtrip_nested_object() {
        let inner = serde_json::json!({"x": 1, "y": 2});
        let mut map = Map::new();
        map.insert("pos".into(), inner);
        map.insert("name".into(), Value::String("test".into()));
        let binary = map_to_binary(&map);
        let row = BinaryRow::parse(&binary).unwrap();
        let back = row.to_map().unwrap();
        assert_eq!(back, map);
    }

    #[test]
    fn roundtrip_array() {
        let mut map = Map::new();
        map.insert("tags".into(), serde_json::json!(["a", "b", "c"]));
        map.insert("count".into(), Value::Number(3.into()));
        let binary = map_to_binary(&map);
        let row = BinaryRow::parse(&binary).unwrap();
        let back = row.to_map().unwrap();
        assert_eq!(back, map);
    }

    #[test]
    fn class_in_hierarchy() {
        let mut map = Map::new();
        map.insert("__class__".into(), Value::String("Employee".into()));
        map.insert("name".into(), Value::String("Bob".into()));
        let binary = map_to_binary(&map);
        let row = BinaryRow::parse(&binary).unwrap();

        let mut hierarchy = std::collections::HashSet::new();
        hierarchy.insert("Employee".into());
        hierarchy.insert("Person".into());
        assert!(row.class_in_hierarchy(&hierarchy));

        let mut hierarchy2 = std::collections::HashSet::new();
        hierarchy2.insert("Manager".into());
        assert!(!row.class_in_hierarchy(&hierarchy2));
    }

    #[test]
    fn binary_value_eq_serde_works() {
        let mut map = Map::new();
        map.insert("x".into(), Value::Number(42.into()));
        let binary = map_to_binary(&map);
        let row = BinaryRow::parse(&binary).unwrap();
        let (tag, raw) = row.field_value_raw(0);
        assert!(binary_value_eq_serde(tag, raw, &Value::Number(42.into())));
        assert!(!binary_value_eq_serde(tag, raw, &Value::Number(99.into())));
    }
}
