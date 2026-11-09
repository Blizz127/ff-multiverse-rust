//! The decoders the sound files will need are really compiled in. A feature typo in Cargo.toml
//! would otherwise show up only when the first real file is opened, on the owner's machine.
//!
//! Nothing here reads a file: the types below exist only with their features on (so this stops
//! compiling without them), and the codec registry is asked for each codec by id.

use symphonia::core::codecs::{CODEC_TYPE_ADPCM_IMA_WAV, CODEC_TYPE_ADPCM_MS, CODEC_TYPE_PCM_S16LE, CODEC_TYPE_VORBIS};
use symphonia::default::codecs::{AdpcmDecoder, PcmDecoder, VorbisDecoder};
use symphonia::default::formats::{OggReader, WavReader};

#[test]
fn ogg_vorbis_wav_pcm_and_adpcm_are_compiled_in() {
    // The containers and decoders exist as types...
    for name in [
        std::any::type_name::<OggReader>(),
        std::any::type_name::<WavReader>(),
        std::any::type_name::<VorbisDecoder>(),
        std::any::type_name::<PcmDecoder>(),
        std::any::type_name::<AdpcmDecoder>(),
    ] {
        assert!(name.starts_with("symphonia"), "{name}");
    }
    // ...and the default registry hands out a decoder for each codec the files use: Vorbis for the
    // music, PCM for the loose WAV files and (MS or IMA) ADPCM for the packed sound effects.
    let codecs = symphonia::default::get_codecs();
    for (codec, what) in [
        (CODEC_TYPE_VORBIS, "Vorbis"),
        (CODEC_TYPE_PCM_S16LE, "16-bit PCM"),
        (CODEC_TYPE_ADPCM_MS, "Microsoft ADPCM"),
        (CODEC_TYPE_ADPCM_IMA_WAV, "IMA ADPCM"),
    ] {
        assert!(codecs.get_codec(codec).is_some(), "no {what} decoder is registered");
    }
}
