//! Minimal HRR (Holographic Reduced Representations) port for native memory.
//!
//! Copyright (c) Nous Research / Hermes Agent contributors.
//! SPDX-License-Identifier: MIT
//!
//! Derived from upstream `plugins/memory/holographic/holographic.py` at
//! commit `6f7a799` — phase encoding, deterministic SHA-256 atoms, bind/unbind/
//! bundle, similarity, and float32-prefixed blob serialization.

use sha2::{Digest, Sha256};

pub const HRR_DIM: usize = 1024;
const TWO_PI: f64 = std::f64::consts::TAU;
const FLOAT32_BLOB_PREFIX: &[u8] = b"HRR1";

pub const ROLE_CONTENT: &str = "__hrr_role_content__";
pub const ROLE_ENTITY: &str = "__hrr_role_entity__";

/// Deterministic phase vector: SHA-256 counter blocks of `"{word}:{i}"` → uint16 → [0, 2π).
pub fn encode_atom(word: &str, dim: usize) -> Vec<f64> {
    let blocks = dim.div_ceil(16);
    let mut out = Vec::with_capacity(dim);
    for i in 0..blocks {
        let digest = Sha256::digest(format!("{word}:{i}"));
        for chunk in digest.chunks_exact(2) {
            if out.len() >= dim {
                break;
            }
            let v = u16::from_le_bytes([chunk[0], chunk[1]]);
            out.push(f64::from(v) * (TWO_PI / 65536.0));
        }
    }
    out.truncate(dim);
    out
}

pub fn bind(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x + y).rem_euclid(TWO_PI))
        .collect()
}

pub fn unbind(memory: &[f64], key: &[f64]) -> Vec<f64> {
    memory
        .iter()
        .zip(key.iter())
        .map(|(m, k)| (m - k).rem_euclid(TWO_PI))
        .collect()
}

pub fn bundle(vectors: &[&[f64]]) -> Vec<f64> {
    if vectors.is_empty() {
        return encode_atom("__hrr_empty__", HRR_DIM);
    }
    let dim = vectors[0].len();
    let mut sum_re = vec![0.0f64; dim];
    let mut sum_im = vec![0.0f64; dim];
    for v in vectors {
        for (i, &phase) in v.iter().enumerate() {
            sum_re[i] += phase.cos();
            sum_im[i] += phase.sin();
        }
    }
    sum_re
        .iter()
        .zip(sum_im.iter())
        .map(|(re, im)| im.atan2(*re).rem_euclid(TWO_PI))
        .collect()
}

pub fn similarity(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len()) as f64;
    if n == 0.0 {
        return 0.0;
    }
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).cos())
        .sum::<f64>()
        / n
}

const PUNCT: &[char] = &[
    '.', ',', ';', ':', '!', '?', '"', '\'', '(', ')', '[', ']', '{', '}', '#', '@', '<', '>',
];

fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split_whitespace()
        .map(|w| w.trim_matches(PUNCT).to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

pub fn encode_text(text: &str, dim: usize) -> Vec<f64> {
    let tokens = tokenize(text);
    if tokens.is_empty() {
        return encode_atom("__hrr_empty__", dim);
    }
    let refs: Vec<Vec<f64>> = tokens.iter().map(|t| encode_atom(t, dim)).collect();
    let ptrs: Vec<&[f64]> = refs.iter().map(|v| v.as_slice()).collect();
    bundle(&ptrs)
}

pub fn encode_fact(content: &str, entities: &[String], dim: usize) -> Vec<f64> {
    let role_content = encode_atom(ROLE_CONTENT, dim);
    let role_entity = encode_atom(ROLE_ENTITY, dim);
    let content_vec = bind(&encode_text(content, dim), &role_content);
    let mut parts: Vec<Vec<f64>> = vec![content_vec];
    for entity in entities {
        parts.push(bind(
            &encode_atom(&entity.to_lowercase(), dim),
            &role_entity,
        ));
    }
    let ptrs: Vec<&[f64]> = parts.iter().map(|v| v.as_slice()).collect();
    bundle(&ptrs)
}

pub fn phases_to_bytes(phases: &[f64], dim: usize) -> Vec<u8> {
    let float32_bytes = FLOAT32_BLOB_PREFIX.len() + dim * 4;
    let float64_bytes = dim * 8;
    if float32_bytes == float64_bytes {
        let mut out = Vec::with_capacity(float64_bytes);
        for p in phases.iter().take(dim) {
            out.extend_from_slice(&p.to_le_bytes());
        }
        return out;
    }
    let mut out = Vec::with_capacity(float32_bytes);
    out.extend_from_slice(FLOAT32_BLOB_PREFIX);
    for p in phases.iter().take(dim) {
        out.extend_from_slice(&(*p as f32).to_le_bytes());
    }
    out
}

pub fn bytes_to_phases(data: &[u8], dim: usize) -> Result<Vec<f64>, String> {
    let plen = FLOAT32_BLOB_PREFIX.len();
    let prefixed = data.starts_with(FLOAT32_BLOB_PREFIX);
    let float32_blob_bytes = plen + dim * 4;
    let float64_bytes = dim * 8;
    let collides = float32_blob_bytes == float64_bytes;

    if !collides && prefixed && data.len() == float32_blob_bytes {
        let payload = &data[plen..];
        return Ok(payload
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64)
            .collect());
    }
    if data.len() == float64_bytes {
        return Ok(data
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]))
            .collect());
    }
    Err(format!(
        "HRR vector blob has {} bytes; expected prefixed float32 or legacy float64 for dim={dim}",
        data.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_atom_matches_upstream_snapshot() {
        let v = encode_atom("hello", 4);
        assert!((v[0] - 5.946572).abs() < 1e-4);
        assert!((v[1] - 5.294247).abs() < 1e-4);
        assert!((v[2] - 4.877388).abs() < 1e-4);
        assert!((v[3] - 2.842562).abs() < 1e-4);
    }

    #[test]
    fn blob_roundtrip_1024() {
        let phases = encode_atom("roundtrip-probe", HRR_DIM);
        let blob = phases_to_bytes(&phases, HRR_DIM);
        let back = bytes_to_phases(&blob, HRR_DIM).expect("parse");
        for (a, b) in phases.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-5);
        }
    }
}
