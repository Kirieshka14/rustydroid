#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt};
use std::collections::HashMap;
use std::io::Cursor;

/// Represents an XML attribute in an Android Binary XML file.
#[derive(Debug, Clone)]
pub struct AxmlAttribute {
    pub uri: String,
    pub name: String,
    pub value_str: Option<String>,
    pub data_type: u32,
    pub data: u32,
}

impl AxmlAttribute {
    pub fn resolved_value(&self, string_pool: &[String]) -> String {
        match self.data_type {
            // TYPE_REFERENCE
            0x01 => format!("@0x{:08x}", self.data),
            // TYPE_STRING
            0x03 => {
                if let Some(ref s) = self.value_str {
                    if !s.is_empty() {
                        return s.clone();
                    }
                }
                if (self.data as usize) < string_pool.len() {
                    string_pool[self.data as usize].clone()
                } else {
                    format!("string_idx_{}", self.data)
                }
            }
            // TYPE_INT_DEC
            0x10 => (self.data as i32).to_string(),
            // TYPE_INT_HEX
            0x11 => format!("0x{:08x}", self.data),
            // TYPE_INT_BOOLEAN
            0x12 => if self.data != 0 { "true" } else { "false" }.to_string(),
            _ => {
                if let Some(ref s) = self.value_str {
                    if !s.is_empty() {
                        return s.clone();
                    }
                }
                format!("0x{:08x}", self.data)
            }
        }
    }
}

/// Represents an XML element in AXML.
#[derive(Debug, Clone)]
pub struct AxmlElement {
    pub name: String,
    pub attributes: HashMap<String, AxmlAttribute>,
    pub children: Vec<AxmlElement>,
}

impl AxmlElement {
    pub fn get_attr(&self, attr_name: &str, string_pool: &[String]) -> Option<String> {
        self.attributes
            .get(attr_name)
            .or_else(|| {
                let stripped = attr_name.strip_prefix("android:").unwrap_or(attr_name);
                self.attributes.get(stripped)
            })
            .or_else(|| {
                let with_prefix = format!("android:{}", attr_name);
                self.attributes.get(&with_prefix)
            })
            .map(|a| a.resolved_value(string_pool))
    }

    pub fn find_children(&self, tag_name: &str) -> Vec<&AxmlElement> {
        self.children.iter().filter(|c| c.name == tag_name).collect()
    }
}

const RES_STRING_POOL_TYPE: u16 = 0x0001;
const RES_XML_RESOURCE_MAP_TYPE: u16 = 0x0180;
const RES_XML_START_NAMESPACE_TYPE: u16 = 0x0100;
const RES_XML_END_NAMESPACE_TYPE: u16 = 0x0101;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
const RES_XML_END_ELEMENT_TYPE: u16 = 0x0103;
const RES_XML_CDATA_TYPE: u16 = 0x0104;

pub struct AxmlParser<'a> {
    data: &'a [u8],
    string_pool: Vec<String>,
}

impl<'a> AxmlParser<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            string_pool: Vec::new(),
        }
    }

    pub fn parse(&mut self) -> Result<AxmlElement> {
        if self.data.len() < 8 {
            bail!("AXML data too short (less than 8 bytes)");
        }

        let mut cursor = Cursor::new(self.data);
        let magic = cursor.read_u32::<LittleEndian>()?;
        if (magic & 0xFFFF) != 0x0003 {
            bail!("Invalid AXML magic: 0x{:08x}", magic);
        }
        let _file_size = cursor.read_u32::<LittleEndian>()?;

        let mut root_elements: Vec<AxmlElement> = Vec::new();
        let mut element_stack: Vec<AxmlElement> = Vec::new();

        while (cursor.position() as usize) < self.data.len() {
            let chunk_pos = cursor.position() as usize;
            if chunk_pos + 8 > self.data.len() {
                break;
            }

            let chunk_type_raw = cursor.read_u32::<LittleEndian>()?;
            let chunk_size = cursor.read_u32::<LittleEndian>()? as usize;

            if chunk_size < 8 || chunk_pos.checked_add(chunk_size).map_or(true, |end| end > self.data.len()) {
                break;
            }

            let next_chunk_pos = chunk_pos + chunk_size;
            let chunk_type = (chunk_type_raw & 0xFFFF) as u16;

            match chunk_type {
                RES_STRING_POOL_TYPE => {
                    self.parse_string_pool(&self.data[chunk_pos..next_chunk_pos])?;
                }
                RES_XML_RESOURCE_MAP_TYPE => {}
                RES_XML_START_NAMESPACE_TYPE | RES_XML_END_NAMESPACE_TYPE => {}
                RES_XML_START_ELEMENT_TYPE => {
                    let _line_number = cursor.read_u32::<LittleEndian>()?;
                    let _comment = cursor.read_u32::<LittleEndian>()?;
                    let _ns_idx = cursor.read_u32::<LittleEndian>()?;
                    let name_idx = cursor.read_u32::<LittleEndian>()?;
                    let _attr_start = cursor.read_u16::<LittleEndian>()?;
                    let _attr_size = cursor.read_u16::<LittleEndian>()?;
                    let attr_count = cursor.read_u16::<LittleEndian>()? as usize;
                    let _id_idx = cursor.read_u16::<LittleEndian>()?;
                    let _class_idx = cursor.read_u16::<LittleEndian>()?;
                    let _style_idx = cursor.read_u16::<LittleEndian>()?;

                    let tag_name = self.get_string(name_idx).unwrap_or_else(|| format!("tag_{}", name_idx));
                    let mut attributes = HashMap::new();

                    for _ in 0..attr_count {
                        let attr_ns_idx = cursor.read_u32::<LittleEndian>()?;
                        let attr_name_idx = cursor.read_u32::<LittleEndian>()?;
                        let attr_val_idx = cursor.read_u32::<LittleEndian>()?;
                        let attr_type = cursor.read_u32::<LittleEndian>()? >> 24;
                        let attr_data = cursor.read_u32::<LittleEndian>()?;

                        let uri = self.get_string(attr_ns_idx).unwrap_or_default();
                        let name = self.get_string(attr_name_idx).unwrap_or_else(|| format!("attr_{}", attr_name_idx));
                        let value_str = self.get_string(attr_val_idx);

                        attributes.insert(
                            name.clone(),
                            AxmlAttribute {
                                uri,
                                name,
                                value_str,
                                data_type: attr_type,
                                data: attr_data,
                            },
                        );
                    }

                    element_stack.push(AxmlElement {
                        name: tag_name,
                        attributes,
                        children: Vec::new(),
                    });
                }
                RES_XML_END_ELEMENT_TYPE => {
                    if let Some(finished_elem) = element_stack.pop() {
                        if let Some(parent) = element_stack.last_mut() {
                            parent.children.push(finished_elem);
                        } else {
                            root_elements.push(finished_elem);
                        }
                    }
                }
                RES_XML_CDATA_TYPE => {}
                _ => {}
            }

            cursor.set_position(next_chunk_pos as u64);
        }

        root_elements
            .into_iter()
            .next()
            .context("No root element found in AXML")
    }

    fn parse_string_pool(&mut self, chunk_bytes: &[u8]) -> Result<()> {
        if chunk_bytes.len() < 28 {
            bail!("String pool chunk too small (< 28 bytes)");
        }
        let mut cursor = Cursor::new(chunk_bytes);
        let _chunk_type = cursor.read_u32::<LittleEndian>()?;
        let _chunk_size = cursor.read_u32::<LittleEndian>()?;
        let string_count = cursor.read_u32::<LittleEndian>()? as usize;
        let _style_count = cursor.read_u32::<LittleEndian>()?;
        let flags = cursor.read_u32::<LittleEndian>()?;
        let strings_start = cursor.read_u32::<LittleEndian>()? as usize;
        let _styles_start = cursor.read_u32::<LittleEndian>()?;

        let is_utf8 = (flags & (1 << 8)) != 0;

        let max_possible_strings = (chunk_bytes.len().saturating_sub(28)) / 4;
        let count = std::cmp::min(string_count, max_possible_strings);

        let mut offsets = Vec::with_capacity(count);
        for _ in 0..count {
            offsets.push(cursor.read_u32::<LittleEndian>()? as usize);
        }

        self.string_pool.clear();
        for offset in offsets {
            let str_abs_offset = strings_start.saturating_add(offset);
            if str_abs_offset >= chunk_bytes.len() {
                self.string_pool.push(String::new());
                continue;
            }

            let str_slice = &chunk_bytes[str_abs_offset..];
            let s = if is_utf8 {
                Self::decode_utf8_string(str_slice)
            } else {
                Self::decode_utf16_string(str_slice)
            };
            self.string_pool.push(s);
        }

        Ok(())
    }

    pub fn decode_utf8_string(bytes: &[u8]) -> String {
        if bytes.is_empty() {
            return String::new();
        }
        let mut idx = 0;

        // 1. Decode UTF-16 code unit length (char count)
        if idx >= bytes.len() {
            return String::new();
        }
        let b0 = bytes[idx];
        idx += 1;
        let _char_count = if (b0 & 0x80) != 0 {
            if idx >= bytes.len() {
                return String::new();
            }
            let b1 = bytes[idx];
            idx += 1;
            (((b0 & 0x7F) as usize) << 8) | (b1 as usize)
        } else {
            b0 as usize
        };

        // 2. Decode UTF-8 byte length
        if idx >= bytes.len() {
            return String::new();
        }
        let b2 = bytes[idx];
        idx += 1;
        let byte_len = if (b2 & 0x80) != 0 {
            if idx >= bytes.len() {
                return String::new();
            }
            let b3 = bytes[idx];
            idx += 1;
            (((b2 & 0x7F) as usize) << 8) | (b3 as usize)
        } else {
            b2 as usize
        };

        // 3. Extract byte slice
        if idx + byte_len <= bytes.len() {
            String::from_utf8_lossy(&bytes[idx..idx + byte_len]).to_string()
        } else {
            let available = &bytes[idx..];
            let end = available.iter().position(|&b| b == 0).unwrap_or(available.len());
            String::from_utf8_lossy(&available[..end]).to_string()
        }
    }

    pub fn decode_utf16_string(bytes: &[u8]) -> String {
        if bytes.len() < 2 {
            return String::new();
        }
        let mut cursor = Cursor::new(bytes);
        let len = match cursor.read_u16::<LittleEndian>() {
            Ok(l) => {
                if (l & 0x8000) != 0 {
                    let l2 = cursor.read_u16::<LittleEndian>().unwrap_or(0);
                    (((l & 0x7FFF) as usize) << 16) | (l2 as usize)
                } else {
                    l as usize
                }
            }
            Err(_) => return String::new(),
        };

        let max_possible_chars = (bytes.len().saturating_sub(cursor.position() as usize)) / 2;
        let cap = std::cmp::min(len, max_possible_chars);

        let mut u16_chars = Vec::with_capacity(cap);
        for _ in 0..len {
            if let Ok(c) = cursor.read_u16::<LittleEndian>() {
                if c == 0 {
                    break;
                }
                u16_chars.push(c);
            } else {
                break;
            }
        }

        String::from_utf16_lossy(&u16_chars)
    }

    pub fn get_string(&self, index: u32) -> Option<String> {
        self.string_pool.get(index as usize).cloned()
    }

    pub fn string_pool(&self) -> &[String] {
        &self.string_pool
    }
}
