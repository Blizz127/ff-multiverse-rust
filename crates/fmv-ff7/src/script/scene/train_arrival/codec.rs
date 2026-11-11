//! Lossless retained input for the reviewed train arrival. This record stores the original
//! story, not the later story after talks have changed it. Entry scratch is reconstructed
//! only by the explicit train-entry profile. A checksum detects changed bytes; it is not
//! authorization or proof that a player actually entered the field.
//!
//! The containing visit transaction must bind this immutable record to its accepted entry.
//! Presentation clock history, when needed, belongs to the ready anchor separately.

use crate::field::FieldFile;
use crate::story::{StoryState, MAX_NATIVE_PARTY_BLOB_BYTES};
use super::{replay_train_arrival, train_arrival_ready};
use super::super::{ReplayIdentity, SceneError, SceneInput, SceneJump, ScenePlay,
    SceneResources, WindowAck, WindowCheckpoint};

const MAGIC: &[u8; 8] = b"FMVTRAIN";
const VERSION: u16 = 1;
const DIGEST_BYTES: usize = 32;
const DOMAIN: &[u8] = b"fmv-ff7/train-arrival-record\0v1\0";
/// The complete largest canonical story plus fixed identities and two bounded window ACKs.
/// No compression, known-bit dropping or selected-bank approximation is used.
pub const MAX_TRAIN_ARRIVAL_RECORD_BYTES: usize = MAX_NATIVE_PARTY_BLOB_BYTES + 256;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum TrainArrivalRecordError {
    #[error("the retained train-arrival input is malformed, noncanonical or too large")]
    Encoding,
    #[error("the retained train-arrival checksum or source identity differs")]
    Identity,
    #[error(transparent)]
    Scene(#[from] SceneError),
}

type Result<T> = std::result::Result<T, TrainArrivalRecordError>;

/// Encode only an internally executed, strictly completed train entry. Publicly constructed
/// outcomes cannot supply the story, scratch or transcript used to mint this record.
pub fn encode_train_arrival(play: &ScenePlay) -> Result<Vec<u8>> {
    train_arrival_ready(play)?;
    let entry = play.train_entry().ok_or(TrainArrivalRecordError::Encoding)?;
    Record {
        source: entry.source_identity(), identity: play.replay_identity(),
        story: entry.original_story().clone(), resources: entry.resources(),
        landing: entry.landing(), transcript: play.transcript().to_vec(),
    }.encode()
}

/// Reconstruct the completed interpreter from the exact original input and both accepted
/// window identities. The actual field/profile and every terminal effect are checked again.
/// This does not activate a client, emit audio, or accept the containing visit's provenance.
pub fn replay_train_arrival_record(field: &FieldFile, bytes: &[u8]) -> Result<ScenePlay> {
    let record = Record::decode(bytes)?;
    let play = replay_train_arrival(field, &record.story, record.resources, record.landing,
        record.identity, &record.transcript)?;
    if play.train_entry().is_none_or(|entry| entry.source_identity() != record.source) {
        return Err(TrainArrivalRecordError::Identity);
    }
    train_arrival_ready(&play)?;
    Ok(play)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    source: [u8; 32],
    identity: ReplayIdentity,
    story: StoryState,
    resources: SceneResources,
    landing: SceneJump,
    transcript: Vec<SceneInput>,
}

impl Record {
    fn encode(&self) -> Result<Vec<u8>> {
        let story = self.story.encode();
        if !self.story.supports_extended() || story.len() > MAX_NATIVE_PARTY_BLOB_BYTES
            || self.source == [0; 32] || self.identity.fingerprint == [0; 32]
            || self.transcript.len() != 2 {
            return Err(TrainArrivalRecordError::Encoding);
        }
        let gil = self.resources.gil.ok_or(TrainArrivalRecordError::Encoding)?;
        let mut out = Vec::with_capacity(story.len() + 256);
        out.extend(MAGIC); out.extend(VERSION.to_le_bytes());
        out.extend(self.source); out.extend(self.identity.fingerprint);
        out.extend(self.identity.version.to_le_bytes()); out.extend(self.identity.seed.to_le_bytes());
        out.extend((story.len() as u16).to_le_bytes()); out.extend(story);
        out.extend(gil.to_le_bytes()); out.extend(self.landing.field.to_le_bytes());
        out.extend(self.landing.x.to_le_bytes()); out.extend(self.landing.y.to_le_bytes());
        out.extend(self.landing.triangle.to_le_bytes()); out.push(self.landing.direction);
        out.push(2);
        for event in &self.transcript {
            let SceneInput::Window(ack) = event else { return Err(TrainArrivalRecordError::Encoding) };
            let mark = ack.checkpoint;
            out.extend(mark.ordinal.to_le_bytes()); out.push(mark.entity);
            out.push(u8::from(mark.instruction_offset.is_some()));
            if let Some(offset) = mark.instruction_offset { out.extend(offset.to_le_bytes()); }
            out.push(mark.window); out.push(u8::from(mark.dialog.is_some()));
            if let Some(dialog) = mark.dialog { out.extend(dialog.to_le_bytes()); }
            out.push(u8::from(mark.choices.is_some()));
            if let Some(choices) = mark.choices { out.extend(choices); }
            out.push(u8::from(ack.answer.is_some()));
            if let Some(answer) = ack.answer { out.push(answer); }
        }
        out.extend(digest(&out));
        if out.len() > MAX_TRAIN_ARRIVAL_RECORD_BYTES { return Err(TrainArrivalRecordError::Encoding); }
        Ok(out)
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_TRAIN_ARRIVAL_RECORD_BYTES || bytes.len() < MAGIC.len() + 2 + DIGEST_BYTES {
            return Err(TrainArrivalRecordError::Encoding);
        }
        let (body, checksum) = bytes.split_at(bytes.len() - DIGEST_BYTES);
        if digest(body) != checksum { return Err(TrainArrivalRecordError::Identity); }
        let mut input = Input(body);
        if input.take(8)? != MAGIC || input.u16()? != VERSION { return Err(TrainArrivalRecordError::Encoding); }
        let source = input.array()?;
        let fingerprint = input.array()?;
        let identity = ReplayIdentity { fingerprint, version: input.u32()?, seed: input.u32()? };
        let len = usize::from(input.u16()?);
        if len > MAX_NATIVE_PARTY_BLOB_BYTES { return Err(TrainArrivalRecordError::Encoding); }
        let blob = input.take(len)?;
        let story = StoryState::decode(blob).filter(StoryState::supports_extended)
            .ok_or(TrainArrivalRecordError::Encoding)?;
        if story.encode() != blob { return Err(TrainArrivalRecordError::Encoding); }
        let resources = SceneResources { gil: Some(input.u32()?) };
        let landing = SceneJump { field: input.u16()?, x: input.i16()?, y: input.i16()?,
            triangle: input.i16()?, direction: input.byte()? };
        if input.byte()? != 2 { return Err(TrainArrivalRecordError::Encoding); }
        let mut transcript = Vec::with_capacity(2);
        for _ in 0..2 {
            let ordinal = input.u32()?;
            let entity = input.byte()?;
            let instruction_offset = if input.flag()? { Some(input.u32()?) } else { None };
            let window = input.byte()?;
            let dialog = if input.flag()? { Some(input.u16()?) } else { None };
            let choices = if input.flag()? { Some(input.array()?) } else { None };
            let answer = if input.flag()? { Some(input.byte()?) } else { None };
            transcript.push(SceneInput::Window(WindowAck {
                checkpoint: WindowCheckpoint { ordinal, entity, instruction_offset, window, dialog, choices }, answer,
            }));
        }
        if !input.0.is_empty() { return Err(TrainArrivalRecordError::Encoding); }
        let record = Self { source, identity, story, resources, landing, transcript };
        if record.encode()? != bytes { return Err(TrainArrivalRecordError::Encoding); }
        Ok(record)
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(DOMAIN); hash.update(bytes);
    *hash.finalize().as_bytes()
}

struct Input<'a>(&'a [u8]);
impl<'a> Input<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let (bytes, rest) = self.0.split_at_checked(count).ok_or(TrainArrivalRecordError::Encoding)?;
        self.0 = rest;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| TrainArrivalRecordError::Encoding)
    }
    fn byte(&mut self) -> Result<u8> { Ok(self.array::<1>()?[0]) }
    fn flag(&mut self) -> Result<bool> {
        match self.byte()? { 0 => Ok(false), 1 => Ok(true), _ => Err(TrainArrivalRecordError::Encoding) }
    }
    fn u16(&mut self) -> Result<u16> { Ok(u16::from_le_bytes(self.array()?)) }
    fn i16(&mut self) -> Result<i16> { Ok(i16::from_le_bytes(self.array()?)) }
    fn u32(&mut self) -> Result<u32> { Ok(u32::from_le_bytes(self.array()?)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(story: StoryState) -> Record {
        Record { source: [7; 32], identity: ReplayIdentity { version: 3, fingerprint: [9; 32], seed: 17 },
            story, resources: SceneResources { gil: Some(333) },
            landing: SceneJump { field: 100, x: -2, y: 608, triangle: 29, direction: 252 },
            transcript: (1..=2).map(|ordinal| SceneInput::Window(WindowAck {
                checkpoint: WindowCheckpoint { ordinal, entity: 17, instruction_offset: Some(4100 + ordinal),
                    window: 0, dialog: Some(ordinal as u16), choices: None }, answer: None,
            })).collect() }
    }

    #[test]
    fn full_native_story_and_partial_knowledge_round_trip_without_projection_or_compression() {
        let mut full = StoryState::at(48).into_native_party().unwrap();
        for bank in [1, 3, 7, 11, 13] {
            for address in 0..=255u8 { full.write_byte(bank, address, address.wrapping_add(bank)).unwrap(); }
        }
        full.write_word(2, 0, 48).unwrap();
        assert_eq!(full.encode().len(), MAX_NATIVE_PARTY_BLOB_BYTES);
        assert!(fixture(full.clone()).encode().unwrap().len() > 1024,
            "a complete original story cannot fit the old entry payload bound");
        let mut partial = StoryState::at(48).into_extended().unwrap();
        partial.write_masked(3, 223, 4, 5).unwrap();
        partial.write_masked(13, 101, 128, 128).unwrap();
        for story in [full, partial] {
            let record = fixture(story);
            let bytes = record.encode().unwrap();
            assert!(bytes.len() <= MAX_TRAIN_ARRIVAL_RECORD_BYTES);
            assert_eq!(Record::decode(&bytes).unwrap(), record);
            assert_eq!(Record::decode(&bytes).unwrap().encode().unwrap(), bytes);
        }
    }

    #[test]
    fn truncation_corruption_extra_bytes_and_noncanonical_headers_refuse() {
        let record = fixture(StoryState::at(48).into_extended().unwrap());
        let original = record.encode().unwrap();
        for end in 0..original.len() { assert!(Record::decode(&original[..end]).is_err()); }
        for at in 0..original.len() {
            let mut bytes = original.clone(); bytes[at] ^= 1;
            assert!(Record::decode(&bytes).is_err(), "changed byte {at}");
        }
        // These have valid checksums: source-independent structural checks still apply.
        let body = &original[..original.len() - DIGEST_BYTES];
        let mut trailing = body.to_vec(); trailing.push(0);
        let mut version = body.to_vec(); version[8] = 2;
        let mut count = body.to_vec(); count[97 + record.story.encode().len()] = 3;
        let mut flag = body.to_vec(); flag[103 + record.story.encode().len()] = 2;
        let mut length = body.to_vec(); length[82..84].copy_from_slice(&u16::MAX.to_le_bytes());
        for mut bytes in [trailing, version, count, flag, length] {
            bytes.extend(digest(&bytes));
            assert!(Record::decode(&bytes).is_err());
        }
        assert!(Record::decode(&vec![0; MAX_TRAIN_ARRIVAL_RECORD_BYTES + 1]).is_err());
    }
}
