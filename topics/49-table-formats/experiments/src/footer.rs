//! Lane 1c — what it costs to learn where 10 columns are in a wide Parquet
//! file.
//!
//! F3 (Zeng et al., SIGMOD'26 cycle, §4 and §7.2) argues that Parquet's
//! footer is the wrong shape for wide tables: it is one Thrift-compact
//! struct, and Thrift has no random access, "Consequently, the file reader
//! must deserialize the entire metadata segment to access a specific
//! column". Its Figure 9 times metadata for a 64K-row table of floats,
//! projecting 10 columns, as the total column count grows.
//!
//! This module writes the same shape of file with the `parquet` crate
//! (59.1.0, the version the arrow-rs pin declares) and measures the footer:
//! its size in bytes — a count, deterministic for the seeded values — and
//! how long the crate takes to decode it three ways:
//!
//! - **full**: `ParquetMetaDataReader::decode_metadata`;
//! - **skip stats**: `ParquetMetaDataOptions` told to skip column, encoding
//!   and size statistics for every column except the 10 projected — the
//!   closest the crate comes to "decode only what I need";
//! - **skip stats + cached schema**: the same, with the schema descriptor
//!   supplied so the footer's schema is skipped too.
//!
//! Skipping still *walks* every column chunk's bytes; that is the claim
//! lane 1c tests.

use parquet::basic::Type as PhysicalType;
use parquet::data_type::FloatType;
use parquet::file::metadata::{ParquetMetaDataOptions, ParquetMetaDataReader, ParquetStatisticsPolicy};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::types::{SchemaDescriptor, Type};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::sync::Arc;

/// Write a one-row-group Parquet file of `cols` required float columns and
/// `rows` rows into memory and return the whole file.
pub fn write_wide_file(cols: usize, rows: usize, seed: u64) -> Vec<u8> {
    let fields: Vec<Arc<Type>> = (0..cols)
        .map(|i| {
            Arc::new(
                Type::primitive_type_builder(&format!("c{i}"), PhysicalType::FLOAT)
                    .with_repetition(parquet::basic::Repetition::REQUIRED)
                    .build()
                    .unwrap(),
            )
        })
        .collect();
    let schema = Arc::new(Type::group_type_builder("m").with_fields(fields).build().unwrap());
    let props = Arc::new(WriterProperties::builder().build());
    let mut out = Vec::new();
    {
        let mut w = SerializedFileWriter::new(&mut out, schema, props).unwrap();
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut vals = vec![0f32; rows];
        let mut rg = w.next_row_group().unwrap();
        while let Some(mut col) = rg.next_column().unwrap() {
            for v in vals.iter_mut() {
                *v = rng.gen_range(-1.0e3..1.0e3);
            }
            col.typed::<FloatType>().write_batch(&vals, None, None).unwrap();
            col.close().unwrap();
        }
        rg.close().unwrap();
        w.close().unwrap();
    }
    out
}

/// The footer: the Thrift-encoded `FileMetaData` between the data and the
/// trailing `<4-byte length>PAR1`.
pub fn footer_bytes(file: &[u8]) -> &[u8] {
    let n = file.len();
    assert_eq!(&file[n - 4..], b"PAR1");
    let len = u32::from_le_bytes(file[n - 8..n - 4].try_into().unwrap()) as usize;
    &file[n - 8 - len..n - 8]
}

/// Decode modes lane 1c compares.
#[derive(Clone, Copy, Debug)]
pub enum Decode {
    Full,
    SkipStats,
    SkipStatsCachedSchema,
}

/// Options that skip every statistics field except for the `keep` columns,
/// optionally with a pre-parsed schema.
pub fn options(keep: &[usize], schema: Option<Arc<SchemaDescriptor>>) -> ParquetMetaDataOptions {
    let mut o = ParquetMetaDataOptions::new()
        .with_column_stats_policy(ParquetStatisticsPolicy::skip_except(keep))
        .with_encoding_stats_policy(ParquetStatisticsPolicy::skip_except(keep))
        .with_size_stats_policy(ParquetStatisticsPolicy::skip_except(keep));
    if let Some(s) = schema {
        o = o.with_schema(s);
    }
    o
}

/// Decode once; returns the number of column chunks the metadata describes
/// (so the work cannot be optimised away) and the number with statistics.
pub fn decode(footer: &[u8], mode: Decode, opts: &ParquetMetaDataOptions) -> (usize, usize) {
    let md = match mode {
        Decode::Full => ParquetMetaDataReader::decode_metadata(footer).unwrap(),
        _ => ParquetMetaDataReader::decode_metadata_with_options(footer, Some(opts)).unwrap(),
    };
    let rg = md.row_group(0);
    let with_stats = rg.columns().iter().filter(|c| c.statistics().is_some()).count();
    (rg.num_columns(), with_stats)
}

/// `n` distinct seeded column indices out of `cols`, sorted.
pub fn projection(cols: usize, n: usize, seed: u64) -> Vec<usize> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut picked: Vec<usize> = Vec::new();
    while picked.len() < n.min(cols) {
        let c = rng.gen_range(0..cols);
        if !picked.contains(&c) {
            picked.push(c);
        }
    }
    picked.sort();
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_round_trips_and_skip_keeps_only_projected_stats() {
        let f = write_wide_file(50, 256, 1);
        let ft = footer_bytes(&f);
        let keep = projection(50, 10, 2);
        assert_eq!(decode(ft, Decode::Full, &options(&keep, None)), (50, 50));
        let (n, s) = decode(ft, Decode::SkipStats, &options(&keep, None));
        assert_eq!((n, s), (50, 10));
        let schema = ParquetMetaDataReader::decode_schema(ft).unwrap();
        let (n, s) = decode(ft, Decode::SkipStatsCachedSchema, &options(&keep, Some(schema)));
        assert_eq!((n, s), (50, 10));
    }

    #[test]
    fn footer_grows_linearly_with_columns() {
        let a = footer_bytes(&write_wide_file(100, 64, 1)).len();
        let b = footer_bytes(&write_wide_file(200, 64, 1)).len();
        let per_col = (b - a) as f64 / 100.0;
        assert!(per_col > 20.0, "each column chunk adds metadata; got {per_col} B/col");
        assert!((a as f64) > 100.0 * per_col * 0.8);
    }
}
