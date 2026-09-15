// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.

//! Multimedia data support — metadata extraction, content hashing, and search.

use std::collections::HashMap;

/// Supported media types.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaType {
    Image,
    Audio,
    Video,
    Document,
    Archive,
    Unknown,
}

impl MediaType {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tiff" => {
                MediaType::Image
            }
            "mp3" | "wav" | "flac" | "aac" | "ogg" | "wma" | "m4a" => MediaType::Audio,
            "mp4" | "avi" | "mkv" | "mov" | "wmv" | "flv" | "webm" => MediaType::Video,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" => {
                MediaType::Document
            }
            "zip" | "tar" | "gz" | "rar" | "7z" | "bz2" => MediaType::Archive,
            _ => MediaType::Unknown,
        }
    }

    pub fn mime_type(&self, ext: &str) -> String {
        let s = match self {
            MediaType::Image => match ext.to_lowercase().as_str() {
                "jpg" | "jpeg" => "image/jpeg",
                "png" => "image/png",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "svg" => "image/svg+xml",
                "bmp" => "image/bmp",
                _ => "image/octet-stream",
            },
            MediaType::Audio => match ext.to_lowercase().as_str() {
                "mp3" => "audio/mpeg",
                "wav" => "audio/wav",
                "ogg" => "audio/ogg",
                "flac" => "audio/flac",
                _ => "audio/octet-stream",
            },
            MediaType::Video => match ext.to_lowercase().as_str() {
                "mp4" => "video/mp4",
                "webm" => "video/webm",
                "avi" => "video/x-msvideo",
                _ => "video/octet-stream",
            },
            MediaType::Document => match ext.to_lowercase().as_str() {
                "pdf" => "application/pdf",
                "txt" | "md" => "text/plain",
                _ => "application/octet-stream",
            },
            MediaType::Archive => "application/octet-stream",
            MediaType::Unknown => "application/octet-stream",
        };
        s.to_string()
    }
}

/// Multimedia metadata.
#[derive(Debug, Clone)]
pub struct MediaMetadata {
    pub media_id: String,
    pub filename: String,
    pub media_type: MediaType,
    pub mime_type: String,
    pub size_bytes: u64,
    pub content_hash: String, // SHA-256 hex
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub created_at: Option<u64>,
    pub custom: HashMap<String, String>,
}

/// Media registry — stores and searches multimedia metadata.
pub struct MediaRegistry {
    media: HashMap<String, MediaMetadata>,
    hash_index: HashMap<String, String>, // content_hash -> media_id
}

impl MediaRegistry {
    pub fn new() -> Self {
        Self {
            media: HashMap::new(),
            hash_index: HashMap::new(),
        }
    }

    /// Register a media file with metadata.
    pub fn register(&mut self, meta: MediaMetadata) {
        self.hash_index
            .insert(meta.content_hash.clone(), meta.media_id.clone());
        self.media.insert(meta.media_id.clone(), meta);
    }

    /// Get media by ID.
    pub fn get(&self, media_id: &str) -> Option<&MediaMetadata> {
        self.media.get(media_id)
    }

    /// Find media by content hash (deduplication).
    pub fn find_by_hash(&self, hash: &str) -> Option<&MediaMetadata> {
        self.hash_index.get(hash).and_then(|id| self.media.get(id))
    }

    /// Search media by type.
    pub fn search_by_type(&self, media_type: &MediaType) -> Vec<&MediaMetadata> {
        self.media
            .values()
            .filter(|m| &m.media_type == media_type)
            .collect()
    }

    /// Search media by filename pattern (simple contains).
    pub fn search_by_name(&self, pattern: &str) -> Vec<&MediaMetadata> {
        let lower = pattern.to_lowercase();
        self.media
            .values()
            .filter(|m| m.filename.to_lowercase().contains(&lower))
            .collect()
    }

    /// Search by custom attribute.
    pub fn search_by_attr(&self, key: &str, value: &str) -> Vec<&MediaMetadata> {
        self.media
            .values()
            .filter(|m| m.custom.get(key).is_some_and(|v| v == value))
            .collect()
    }

    /// Remove media by ID.
    pub fn remove(&mut self, media_id: &str) -> bool {
        if let Some(meta) = self.media.remove(media_id) {
            self.hash_index.remove(&meta.content_hash);
            true
        } else {
            false
        }
    }

    pub fn count(&self) -> usize {
        self.media.len()
    }

    /// List all media.
    pub fn list(&self) -> Vec<&MediaMetadata> {
        self.media.values().collect()
    }
}

/// Simple SHA-256 hex hash (using built-in).
pub fn content_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(hasher.finalize())
}

// Minimal SHA-256 implementation (no external dependency)
struct Sha256 {
    state: [u32; 8],
    buffer: Vec<u8>,
    total_len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: Vec::new(),
            total_len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
        self.total_len += data.len() as u64;

        while self.buffer.len() >= 64 {
            let block: [u8; 64] = self.buffer[..64].try_into().unwrap();
            self.buffer.drain(..64);
            self.process_block(&block);
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len * 8;
        self.buffer.push(0x80);
        while (self.buffer.len() % 64) != 56 {
            self.buffer.push(0);
        }
        self.buffer.extend_from_slice(&bit_len.to_be_bytes());

        while self.buffer.len() >= 64 {
            let block: [u8; 64] = self.buffer[..64].try_into().unwrap();
            self.buffer.drain(..64);
            self.process_block(&block);
        }

        let mut result = [0u8; 32];
        for (i, &word) in self.state.iter().enumerate() {
            result[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
        }
        result
    }

    fn process_block(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];

        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..(i + 1) * 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

fn hex_encode(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_media_type_from_extension() {
        assert_eq!(MediaType::from_extension("jpg"), MediaType::Image);
        assert_eq!(MediaType::from_extension("PNG"), MediaType::Image);
        assert_eq!(MediaType::from_extension("mp3"), MediaType::Audio);
        assert_eq!(MediaType::from_extension("mp4"), MediaType::Video);
        assert_eq!(MediaType::from_extension("pdf"), MediaType::Document);
        assert_eq!(MediaType::from_extension("zip"), MediaType::Archive);
        assert_eq!(MediaType::from_extension("xyz"), MediaType::Unknown);
    }

    #[test]
    fn test_mime_type() {
        assert_eq!(MediaType::Image.mime_type("jpg"), "image/jpeg");
        assert_eq!(MediaType::Image.mime_type("png"), "image/png");
        assert_eq!(MediaType::Audio.mime_type("mp3"), "audio/mpeg");
        assert_eq!(MediaType::Video.mime_type("mp4"), "video/mp4");
    }

    #[test]
    fn test_media_registry() {
        let mut reg = MediaRegistry::new();
        let meta = MediaMetadata {
            media_id: "m1".into(),
            filename: "photo.jpg".into(),
            media_type: MediaType::Image,
            mime_type: "image/jpeg".into(),
            size_bytes: 1024,
            content_hash: "abc123".into(),
            width: Some(1920),
            height: Some(1080),
            duration_ms: None,
            created_at: None,
            custom: HashMap::new(),
        };
        reg.register(meta);
        assert_eq!(reg.count(), 1);

        let found = reg.get("m1").unwrap();
        assert_eq!(found.filename, "photo.jpg");
        assert_eq!(found.width, Some(1920));
    }

    #[test]
    fn test_find_by_hash() {
        let mut reg = MediaRegistry::new();
        let meta = MediaMetadata {
            media_id: "m1".into(),
            filename: "a.jpg".into(),
            media_type: MediaType::Image,
            mime_type: "image/jpeg".into(),
            size_bytes: 100,
            content_hash: "hash_abc".into(),
            width: None,
            height: None,
            duration_ms: None,
            created_at: None,
            custom: HashMap::new(),
        };
        reg.register(meta);

        assert!(reg.find_by_hash("hash_abc").is_some());
        assert!(reg.find_by_hash("hash_xyz").is_none());
    }

    #[test]
    fn test_search_by_type() {
        let mut reg = MediaRegistry::new();
        reg.register(MediaMetadata {
            media_id: "m1".into(),
            filename: "a.jpg".into(),
            media_type: MediaType::Image,
            mime_type: "image/jpeg".into(),
            size_bytes: 100,
            content_hash: "h1".into(),
            width: None,
            height: None,
            duration_ms: None,
            created_at: None,
            custom: HashMap::new(),
        });
        reg.register(MediaMetadata {
            media_id: "m2".into(),
            filename: "b.mp3".into(),
            media_type: MediaType::Audio,
            mime_type: "audio/mpeg".into(),
            size_bytes: 200,
            content_hash: "h2".into(),
            width: None,
            height: None,
            duration_ms: None,
            created_at: None,
            custom: HashMap::new(),
        });

        let images = reg.search_by_type(&MediaType::Image);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_id, "m1");
    }

    #[test]
    fn test_search_by_name() {
        let mut reg = MediaRegistry::new();
        reg.register(MediaMetadata {
            media_id: "m1".into(),
            filename: "Report_2024.pdf".into(),
            media_type: MediaType::Document,
            mime_type: "application/pdf".into(),
            size_bytes: 100,
            content_hash: "h1".into(),
            width: None,
            height: None,
            duration_ms: None,
            created_at: None,
            custom: HashMap::new(),
        });

        let results = reg.search_by_name("report");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_by_attr() {
        let mut reg = MediaRegistry::new();
        let mut custom = HashMap::new();
        custom.insert("author".into(), "Alice".into());
        reg.register(MediaMetadata {
            media_id: "m1".into(),
            filename: "a.jpg".into(),
            media_type: MediaType::Image,
            mime_type: "image/jpeg".into(),
            size_bytes: 100,
            content_hash: "h1".into(),
            width: None,
            height: None,
            duration_ms: None,
            created_at: None,
            custom,
        });

        let results = reg.search_by_attr("author", "Alice");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].media_id, "m1");
    }

    #[test]
    fn test_remove_media() {
        let mut reg = MediaRegistry::new();
        reg.register(MediaMetadata {
            media_id: "m1".into(),
            filename: "a.jpg".into(),
            media_type: MediaType::Image,
            mime_type: "image/jpeg".into(),
            size_bytes: 100,
            content_hash: "h1".into(),
            width: None,
            height: None,
            duration_ms: None,
            created_at: None,
            custom: HashMap::new(),
        });

        assert!(reg.remove("m1"));
        assert_eq!(reg.count(), 0);
        assert!(reg.find_by_hash("h1").is_none());
    }

    #[test]
    fn test_content_hash() {
        let hash1 = content_hash(b"hello world");
        let hash2 = content_hash(b"hello world");
        let hash3 = content_hash(b"hello world!");
        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64); // SHA-256 hex = 64 chars
    }
}
