//! Reed-Solomon Erasure Coding
//!
//! Splits data into `k` data shards and `m` parity shards.
//! Any `k` out of `k+m` shards reconstructs the original data exactly.
//!
//! This is a real systematic Reed-Solomon code over GF(256) (primitive
//! polynomial `x^8 + x^4 + x^3 + x^2 + 1`, 0x11D): the first `k` rows of
//! the generator matrix are the identity, parity rows come from a
//! Vandermonde construction, and decoding inverts the `k x k` submatrix
//! formed by whichever `k` shards survived. There are no shortcuts —
//! dropping shards either reconstructs the original bytes or fails with
//! `None`.

use crate::shred::Shred;
use std::collections::BTreeMap;

/// Erasure coding configuration
#[derive(Debug, Clone)]
pub struct ErasureConfig {
    /// Number of data shards (k)
    pub data_shards: usize,
    /// Number of parity shards (m)
    pub parity_shards: usize,
}

impl ErasureConfig {
    /// Default Solana-like config: 16 data + 4 parity = 20 total
    pub fn default_solana() -> Self {
        Self {
            data_shards: 16,
            parity_shards: 4,
        }
    }

    /// Total number of shards (k + m)
    pub fn total_shards(&self) -> usize {
        self.data_shards + self.parity_shards
    }

    /// Minimum shards needed to reconstruct
    pub fn min_shards(&self) -> usize {
        self.data_shards
    }
}

/// GF(256) arithmetic for Reed-Solomon coding.
mod gf {
    use std::sync::OnceLock;

    struct Tables {
        exp: [u8; 512],
        log: [u8; 256],
    }

    static TABLES: OnceLock<Tables> = OnceLock::new();

    fn tables() -> &'static Tables {
        TABLES.get_or_init(|| {
            let mut exp = [0u8; 512];
            let mut log = [0u8; 256];
            // Primitive polynomial x^8 + x^4 + x^3 + x^2 + 1
            let mut x: u16 = 1;
            for (i, slot) in exp[..255].iter_mut().enumerate() {
                *slot = x as u8;
                log[x as usize] = i as u8;
                x <<= 1;
                if x & 0x100 != 0 {
                    x ^= 0x11d;
                }
            }
            // Duplicate so exp lookups never need a modulo
            exp.copy_within(0..255, 255);
            Tables { exp, log }
        })
    }

    /// Field multiplication
    pub fn mul(a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            let t = tables();
            t.exp[t.log[a as usize] as usize + t.log[b as usize] as usize]
        }
    }

    /// Field inverse (a != 0)
    pub fn inv(a: u8) -> u8 {
        assert!(a != 0, "GF(256): inverse of zero");
        let t = tables();
        t.exp[255 - t.log[a as usize] as usize]
    }

    /// Exponentiation `base ** e`
    pub fn pow(base: u8, e: usize) -> u8 {
        let mut out = 1u8;
        for _ in 0..e {
            out = mul(out, base);
        }
        out
    }
}

/// Invert a square matrix over GF(256) (Gauss-Jordan).
/// Returns `None` if the matrix is singular.
fn mat_inv(m: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
    let n = m.len();
    debug_assert!(m.iter().all(|row| row.len() == n));

    // Augment with the identity matrix
    let mut a: Vec<Vec<u8>> = m
        .iter()
        .enumerate()
        .map(|(r, row)| {
            let mut aug = row.clone();
            aug.extend((0..n).map(|c| u8::from(c == r)));
            aug
        })
        .collect();

    for col in 0..n {
        // Find a pivot
        let piv = (col..n).find(|&r| a[r][col] != 0)?;
        a.swap(col, piv);

        // Normalize pivot row
        let piv_inv = gf::inv(a[col][col]);
        for v in a[col].iter_mut() {
            *v = gf::mul(*v, piv_inv);
        }

        // Eliminate the column from all other rows
        let pivot_row = a[col].clone();
        for (r, row) in a.iter_mut().enumerate() {
            if r != col && row[col] != 0 {
                let factor = row[col];
                for (v, p) in row.iter_mut().zip(&pivot_row) {
                    *v ^= gf::mul(factor, *p);
                }
            }
        }
    }

    Some(
        a.into_iter()
            .map(|row| row[n..].to_vec())
            .collect::<Vec<_>>(),
    )
}

/// Systematic generator matrix: `(k + m) x k`, top `k` rows = identity.
///
/// Built from a Vandermonde matrix `V[r][c] = r^c` over GF(256) scaled by
/// the inverse of its top `k x k` block, so any `k` rows are linearly
/// independent (Reed-Solomon property).
fn generator(k: usize, m: usize) -> Option<Vec<Vec<u8>>> {
    let v: Vec<Vec<u8>> = (0..k + m)
        .map(|r| (0..k).map(|c| gf::pow(r as u8, c)).collect())
        .collect();

    let top: Vec<Vec<u8>> = v[..k].to_vec();
    let top_inv = mat_inv(&top)?;

    // G = V * top^{-1}
    let mut g = vec![vec![0u8; k]; k + m];
    for (r, grow) in g.iter_mut().enumerate() {
        for (c, gcell) in grow.iter_mut().enumerate() {
            let mut acc = 0u8;
            for t in 0..k {
                acc ^= gf::mul(v[r][t], top_inv[t][c]);
            }
            *gcell = acc;
        }
    }
    Some(g)
}

/// Reed-Solomon erasure coder
pub struct ErasureCoder {
    config: ErasureConfig,
}

impl ErasureCoder {
    /// Create a new erasure coder
    pub fn new(config: ErasureConfig) -> Self {
        Self { config }
    }

    /// Create with default Solana config
    pub fn default_solana() -> Self {
        Self::new(ErasureConfig::default_solana())
    }

    /// Encode data into erasure-coded shreds
    ///
    /// Splits the input into `k` equal data shards (prefixed with the
    /// original length as a little-endian `u64`, zero-padded) and computes
    /// `m` parity shards with the systematic Reed-Solomon generator.
    pub fn encode(&self, data: &[u8], slot: u64) -> Vec<Shred> {
        let k = self.config.data_shards;
        let m = self.config.parity_shards;
        debug_assert!(k > 0 && k + m <= 256);

        // Length prefix so decode can trim the zero padding exactly
        let shard_size = (8 + data.len()).div_ceil(k).max(1);
        let mut buf = Vec::with_capacity(shard_size * k);
        buf.extend_from_slice(&(data.len() as u64).to_le_bytes());
        buf.extend_from_slice(data);
        buf.resize(shard_size * k, 0);

        let g = match generator(k, m) {
            Some(g) => g,
            None => {
                // Singular generator can only happen for a degenerate
                // config (duplicate evaluation points) — fail loudly.
                unreachable!("Vandermonde generator with k+m <= 256 is never singular");
            }
        };

        let mut shards = Vec::with_capacity(k + m);

        // Data shards = the systematic part of the codeword
        for i in 0..k {
            shards.push(Shred::new_data(
                slot,
                i as u64,
                buf[i * shard_size..(i + 1) * shard_size].to_vec(),
            ));
        }

        // Parity shards
        for p in 0..m {
            let row = &g[k + p];
            let mut parity = vec![0u8; shard_size];
            for c in 0..k {
                let coeff = row[c];
                if coeff == 0 {
                    continue;
                }
                for (j, byte) in parity.iter_mut().enumerate() {
                    *byte ^= gf::mul(coeff, buf[c * shard_size + j]);
                }
            }
            shards.push(Shred::new_coding(slot, (k + p) as u64, parity));
        }

        shards
    }

    /// Decode/reconstruct data from shreds
    ///
    /// Takes any `k` shreds (data and/or parity, any indices within
    /// `0..k+m`, order irrelevant) and reconstructs the original bytes
    /// by inverting the generator submatrix formed by the surviving rows.
    /// Returns `None` on any inconsistency — never truncated output.
    pub fn decode(&self, shreds: &[Shred]) -> Option<Vec<u8>> {
        let k = self.config.data_shards;
        let m = self.config.parity_shards;
        let total = k + m;

        // Deduplicate by shred index (first wins), ignore out-of-range
        let mut by_index: BTreeMap<u64, &Shred> = BTreeMap::new();
        for s in shreds {
            if (s.index as usize) < total {
                by_index.entry(s.index).or_insert(s);
            }
        }

        if by_index.len() < k {
            tracing::warn!(
                "Not enough distinct shreds to decode: {} < {}",
                by_index.len(),
                k
            );
            return None;
        }

        // Take exactly k survivors with matching shard size
        let selected: Vec<(u64, &Shred)> = by_index.into_iter().take(k).collect();
        let shard_size = selected[0].1.payload.len();
        if shard_size == 0 || selected.iter().any(|(_, s)| s.payload.len() != shard_size) {
            tracing::warn!("Shred payload lengths mismatch — refusing to decode");
            return None;
        }

        let g = generator(k, m)?;

        // Submatrix of the generator rows that survived
        let sub: Vec<Vec<u8>> = selected
            .iter()
            .map(|(idx, _)| g[*idx as usize].clone())
            .collect();
        let sub_inv = mat_inv(&sub)?;
        debug_assert_eq!(sub_inv.len(), k);

        // Recover the data shards: d = G_sub^{-1} · out (per byte column)
        let mut buf = vec![0u8; k * shard_size];
        for c in 0..k {
            let row = &sub_inv[c];
            for j in 0..shard_size {
                let mut acc = 0u8;
                for (r, (_, s)) in selected.iter().enumerate() {
                    acc ^= gf::mul(row[r], s.payload[j]);
                }
                buf[c * shard_size + j] = acc;
            }
        }

        // Strip the length prefix — anything inconsistent is a hard failure
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(&buf[..8]);
        let original_len = u64::from_le_bytes(len_bytes) as usize;
        let capacity = buf.len() - 8;
        if original_len > capacity {
            tracing::warn!(
                "Reconstructed length prefix {} exceeds shard capacity {}",
                original_len,
                capacity
            );
            return None;
        }

        Some(buf[8..8 + original_len].to_vec())
    }

    /// Get the configuration
    pub fn config(&self) -> &ErasureConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shred::ShredType;

    fn sample_coder() -> ErasureCoder {
        ErasureCoder::new(ErasureConfig {
            data_shards: 4,
            parity_shards: 2,
        })
    }

    /// Every k-subset of the 6 shreds must reconstruct the original data
    fn all_combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        let mut combo = Vec::new();
        fn rec(
            start: usize,
            n: usize,
            k: usize,
            combo: &mut Vec<usize>,
            out: &mut Vec<Vec<usize>>,
        ) {
            if combo.len() == k {
                out.push(combo.clone());
                return;
            }
            for i in start..n {
                combo.push(i);
                rec(i + 1, n, k, combo, out);
                combo.pop();
            }
        }
        rec(0, n, k, &mut combo, &mut out);
        out
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let coder = sample_coder();
        let original_data = b"Hello, Solana Turbine erasure coding!".to_vec();
        let shreds = coder.encode(&original_data, 1);

        assert_eq!(shreds.len(), 6); // 4 data + 2 parity
        assert_eq!(
            shreds
                .iter()
                .filter(|s| s.shred_type == ShredType::Data)
                .count(),
            4
        );
        assert_eq!(
            shreds
                .iter()
                .filter(|s| s.shred_type == ShredType::Coding)
                .count(),
            2
        );

        let decoded = coder.decode(&shreds).expect("full shred set decodes");
        assert_eq!(decoded, original_data);
    }

    #[test]
    fn test_encode_all_shreds_valid() {
        let coder = sample_coder();
        let data = vec![42u8; 1000];
        let shreds = coder.encode(&data, 42);

        for shred in &shreds {
            assert!(shred.verify(), "Shred should be valid");
            assert_eq!(shred.slot, 42);
        }
    }

    #[test]
    fn test_decode_with_enough_shards() {
        let coder = sample_coder();
        let original = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let shreds = coder.encode(&original, 1);

        // Take only data shards (4)
        let data_only: Vec<Shred> = shreds
            .iter()
            .filter(|s| s.shred_type == ShredType::Data)
            .cloned()
            .collect();

        let decoded = coder.decode(&data_only).expect("data shards decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_decode_with_too_few_shards() {
        let coder = sample_coder();
        let data = vec![1u8; 1000];
        let shreds = coder.encode(&data, 1);

        // Take only 2 shreds (less than k=4)
        let few_shreds: Vec<Shred> = shreds.into_iter().take(2).collect();
        let decoded = coder.decode(&few_shreds);
        assert!(decoded.is_none());
    }

    #[test]
    fn test_reconstruct_any_k_of_n() {
        let coder = sample_coder();
        let original: Vec<u8> = (0..123u8).collect(); // odd length on purpose
        let shreds = coder.encode(&original, 7);

        // Drop up to m=2 shreds in EVERY possible combination
        for keep in all_combinations(6, 4) {
            let subset: Vec<Shred> = keep.iter().map(|&i| shreds[i].clone()).collect();
            let decoded = coder
                .decode(&subset)
                .unwrap_or_else(|| panic!("subset {keep:?} must decode"));
            assert_eq!(decoded, original, "subset {keep:?} mismatch");
        }
    }

    #[test]
    fn test_reconstruct_when_data_shards_lost() {
        let coder = sample_coder();
        let original = vec![7u8; 555];
        let shreds = coder.encode(&original, 1);

        // Drop data shards 0 and 1 — must come back from parity alone
        let survivors: Vec<Shred> = shreds
            .iter()
            .filter(|s| !matches!(s.index, 0 | 1))
            .cloned()
            .collect();
        assert_eq!(survivors.len(), 4);

        let decoded = coder.decode(&survivors).expect("reconstruct");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_reconstruct_solana_config() {
        let coder = ErasureCoder::default_solana();
        // 1000 is not a multiple of the shard size — padding must be trimmed
        let original: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let shreds = coder.encode(&original, 42);
        assert_eq!(shreds.len(), 20);

        // Lose 4 shards (m=4): two data + two parity
        let survivors: Vec<Shred> = shreds
            .iter()
            .filter(|s| !matches!(s.index, 0 | 1 | 16 | 17))
            .cloned()
            .collect();
        assert_eq!(survivors.len(), 16);

        let decoded = coder.decode(&survivors).expect("k-of-n reconstruct");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_decode_rejects_mismatched_lengths() {
        let coder = sample_coder();
        let original = vec![9u8; 200];
        let mut shreds = coder.encode(&original, 1);
        // Corrupt one survivor's length
        shreds[2].payload.pop();

        assert!(coder.decode(&shreds).is_none());
    }

    #[test]
    fn test_decode_rejects_bad_length_prefix() {
        let coder = sample_coder();
        let original = vec![9u8; 200];
        let mut shreds = coder.encode(&original, 1);
        // Forge an impossible original length in shard 0
        let forged = (u64::MAX).to_le_bytes();
        shreds[0].payload[..8].copy_from_slice(&forged);

        assert!(coder.decode(&shreds).is_none());
    }

    #[test]
    fn test_default_solana_config() {
        let coder = ErasureCoder::default_solana();
        assert_eq!(coder.config().data_shards, 16);
        assert_eq!(coder.config().parity_shards, 4);
        assert_eq!(coder.config().total_shards(), 20);
    }

    #[test]
    fn test_gf_field_basics() {
        // Every non-zero element has an inverse
        for a in 1..=u8::MAX {
            assert_eq!(gf::mul(a, gf::inv(a)), 1, "a={a}");
        }
        // Distributivity spot-check
        let (a, b, c) = (0x53, 0xCA, 0x42);
        assert_eq!(
            gf::mul(a, b ^ c),
            gf::mul(a, b) ^ gf::mul(a, c),
            "left-distributive"
        );
        // 0 * x = 0, 1 * x = x
        assert_eq!(gf::mul(0, 99), 0);
        assert_eq!(gf::mul(1, 99), 99);
    }

    #[test]
    fn test_generator_is_systematic() {
        let (k, m) = (4usize, 2usize);
        let g = generator(k, m).expect("invertible");
        assert_eq!(g.len(), k + m);
        for (r, row) in g.iter().enumerate().take(k) {
            for (c, cell) in row.iter().enumerate().take(k) {
                assert_eq!(*cell, u8::from(r == c), "top block must be I");
            }
        }
    }
}
