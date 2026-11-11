//! Immutable transport for a complete train-entry record and its initial effect stream.
//!
//! Construction and decoding validate only bounds and encoding. The checksum detects
//! changed bytes; it does not authenticate the record or RNG. The host must establish
//! provenance separately, and checked arrival replay still validates the inner record.

use super::{hash, Input};
use super::super::codec::MAX_TRAIN_ARRIVAL_RECORD_BYTES;

const MAGIC: &[u8; 8] = b"FMVTRINP";
const VERSION: u16 = 1;
const DOMAIN: &[u8] = b"fmv-ff7/train-presentation-entry\0v1\0";
const OVERHEAD: usize = 8 + 2 + 2 + 4 + 32;

/// Maximum complete entry envelope: bounded raw record plus its 48-byte envelope.
pub const MAX_TRAIN_PRESENTATION_ENTRY_BYTES: usize = MAX_TRAIN_ARRIVAL_RECORD_BYTES + OVERHEAD;

/// An immutable, syntactically checked entry envelope, without a provenance claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainPresentationEntry {
    record: Vec<u8>,
    initial_rng: [u8; 2],
}

impl TrainPresentationEntry {
    /// Retain the complete nonempty bounded record and original RNG bytes unchanged.
    /// This does not parse or authenticate the inner arrival record.
    pub fn new(record: &[u8], initial_rng: [u8; 2]) -> Result<Self, String> {
        if record.is_empty() || record.len() > MAX_TRAIN_ARRIVAL_RECORD_BYTES {
            return Err("train presentation entry record exceeds its bound".into());
        }
        Ok(Self { record: record.to_vec(), initial_rng })
    }

    /// The complete separate arrival record, without mutation access.
    pub fn record(&self) -> &[u8] { &self.record }

    /// The initial effect stream retained with this entry.
    pub fn initial_rng(&self) -> [u8; 2] { self.initial_rng }

    /// Encode the versioned envelope with an integrity checksum.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.record.len() + OVERHEAD);
        bytes.extend(MAGIC);
        bytes.extend(VERSION.to_le_bytes());
        bytes.extend(self.initial_rng);
        // The private record is bounded by MAX_TRAIN_ARRIVAL_RECORD_BYTES (5,385).
        bytes.extend((self.record.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&self.record);
        bytes.extend(hash(DOMAIN, &bytes));
        bytes
    }

    /// Check exact framing, bounds, version and checksum, preserving all record bytes.
    /// A caller that recomputes the checksum can replace the RNG or record: provenance
    /// must come from the host's immutable entry, never from this decoder alone.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() <= OVERHEAD || bytes.len() > MAX_TRAIN_PRESENTATION_ENTRY_BYTES {
            return Err("invalid train presentation entry length".into());
        }
        let (body, checksum) = bytes.split_at_checked(bytes.len() - 32)
            .ok_or("truncated train presentation entry checksum")?;
        if hash(DOMAIN, body) != checksum {
            return Err("train presentation entry checksum differs".into());
        }
        let mut input = Input(body);
        if input.take(8)? != MAGIC || u16::from_le_bytes(input.array()?) != VERSION {
            return Err("unsupported train presentation entry encoding".into());
        }
        let initial_rng = input.array()?;
        let len = usize::try_from(u32::from_le_bytes(input.array()?))
            .map_err(|_| "train presentation entry record exceeds this platform")?;
        if len == 0 || len > MAX_TRAIN_ARRIVAL_RECORD_BYTES {
            return Err("train presentation entry record exceeds its bound".into());
        }
        let record = input.take(len)?;
        if !input.0.is_empty() { return Err("train presentation entry has trailing data".into()); }
        let entry = Self::new(record, initial_rng)?;
        if entry.encode() != bytes { return Err("train presentation entry is not canonical".into()); }
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::TrainPresentationCheckpoint;
    use crate::script::scene::presentation::Cast;

    fn signed(mut body: Vec<u8>) -> Vec<u8> {
        body.extend(hash(DOMAIN, &body));
        body
    }

    #[test]
    fn complete_records_and_rng_round_trip_at_both_size_bounds() {
        for len in [1, MAX_TRAIN_ARRIVAL_RECORD_BYTES] {
            let record = (0..len).map(|n| (n % 251) as u8).collect::<Vec<_>>();
            let entry = TrainPresentationEntry::new(&record, [53, 201]).unwrap();
            let bytes = entry.encode();
            assert_eq!(bytes.len(), len + 48);
            assert!(bytes.len() <= MAX_TRAIN_PRESENTATION_ENTRY_BYTES);
            let decoded = TrainPresentationEntry::decode(&bytes).unwrap();
            assert_eq!(decoded, entry);
            assert_eq!(decoded.record(), record);
            assert_eq!(decoded.initial_rng(), [53, 201]);
            assert_eq!(decoded.encode(), bytes);
        }
        assert!(TrainPresentationEntry::new(&[], [0, 0]).is_err());
        assert!(TrainPresentationEntry::new(&vec![0; MAX_TRAIN_ARRIVAL_RECORD_BYTES + 1], [0, 0]).is_err());
        assert!(TrainPresentationEntry::decode(&vec![0; MAX_TRAIN_PRESENTATION_ENTRY_BYTES + 1]).is_err());
    }

    #[test]
    fn truncations_and_changes_without_a_new_checksum_are_rejected() {
        let bytes = TrainPresentationEntry::new(b"complete original entry", [53, 201]).unwrap().encode();
        for end in 0..bytes.len() {
            assert!(TrainPresentationEntry::decode(&bytes[..end]).is_err(), "truncated at {end}");
        }
        for at in 0..bytes.len() {
            let mut changed = bytes.clone();
            changed[at] ^= 1;
            assert!(TrainPresentationEntry::decode(&changed).is_err(), "changed byte {at}");
        }
    }

    #[test]
    fn recomputed_checksums_do_not_admit_bad_headers_lengths_or_trailing_data() {
        let bytes = TrainPresentationEntry::new(b"record", [53, 201]).unwrap().encode();
        let body = &bytes[..bytes.len() - 32];
        let mut cases = Vec::new();
        let mut magic = body.to_vec(); magic[0] ^= 1; cases.push(magic);
        let mut version = body.to_vec(); version[8..10].copy_from_slice(&2u16.to_le_bytes()); cases.push(version);
        for len in [0, 5, 7, MAX_TRAIN_ARRIVAL_RECORD_BYTES as u32 + 1, u32::MAX] {
            let mut length = body.to_vec();
            length[12..16].copy_from_slice(&len.to_le_bytes());
            cases.push(length);
        }
        let mut trailing = body.to_vec(); trailing.push(0); cases.push(trailing);
        for body in cases {
            assert!(TrainPresentationEntry::decode(&signed(body)).is_err());
        }
    }

    #[test]
    fn a_recomputed_checksum_can_replace_rng_but_cannot_rebind_the_original_checkpoint() {
        let entry = TrainPresentationEntry::new(b"separate original record", [53, 201]).unwrap();
        let mut cast = Cast::new([]);
        cast.seed_shake_rng(entry.initial_rng());
        let checkpoint = TrainPresentationCheckpoint::new(entry.record(), entry.initial_rng(),
            0.0, cast.checkpoint().unwrap()).unwrap().encode();
        assert!(TrainPresentationCheckpoint::decode_bound(entry.record(), entry.initial_rng(), &checkpoint).is_ok());

        let bytes = entry.encode();
        let mut body = bytes[..bytes.len() - 32].to_vec();
        body[10] ^= 1;
        let replaced = TrainPresentationEntry::decode(&signed(body)).unwrap();
        assert_eq!(replaced.record(), entry.record());
        assert_eq!(replaced.initial_rng(), [52, 201], "a checksum is not provenance");
        assert!(TrainPresentationCheckpoint::decode_bound(replaced.record(), replaced.initial_rng(), &checkpoint).is_err(),
            "the original checkpoint remains bound to the original immutable entry's RNG");
        let forged = TrainPresentationCheckpoint::new(replaced.record(), replaced.initial_rng(),
            0.0, cast.checkpoint().unwrap()).unwrap().encode();
        assert!(TrainPresentationCheckpoint::decode_bound(entry.record(), entry.initial_rng(), &forged).is_err(),
            "a replacement checkpoint cannot change the host's established entry binding");
    }
}
