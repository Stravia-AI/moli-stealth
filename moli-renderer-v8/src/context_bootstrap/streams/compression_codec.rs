use std::io::{self, ErrorKind, Write};

use brotli::{CompressorWriter, DecompressorWriter};
use flate2::{Compress, Compression, Crc, Decompress, FlushCompress, FlushDecompress, Status};

const OUTPUT_BUFFER_SIZE: usize = 16 * 1024;
const GZIP_HEADER: [u8; 10] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];

pub(in crate::context_bootstrap) struct CompressionCodec {
    state: CodecState,
}

enum CodecState {
    FlateEncoder(FlateEncoder),
    FlateDecoder(FlateDecoder),
    GzipDecoder(GzipDecoder),
    BrotliEncoder(Option<Box<CompressorWriter<Vec<u8>>>>),
    BrotliDecoder(Option<Box<DecompressorWriter<Vec<u8>>>>),
}

#[derive(Clone, Copy)]
enum FlateFormat {
    Deflate,
    DeflateRaw,
    Gzip,
}

struct FlateEncoder {
    compressor: Compress,
    format: FlateFormat,
    gzip_crc: Crc,
    gzip_header_written: bool,
    finished: bool,
}

struct FlateDecoder {
    decompressor: Decompress,
    finished: bool,
}

struct GzipDecoder {
    phase: GzipPhase,
}

enum GzipPhase {
    Header(GzipHeaderParser),
    Body {
        decompressor: Decompress,
        crc: Crc,
    },
    Trailer {
        crc: Crc,
        bytes: [u8; 8],
        filled: usize,
    },
    Finished,
}

struct GzipHeaderParser {
    stage: GzipHeaderStage,
    flags: u8,
    crc: Crc,
}

enum GzipHeaderStage {
    Fixed { bytes: [u8; 10], filled: usize },
    ExtraLength { bytes: [u8; 2], filled: usize },
    Extra { remaining: usize },
    Name,
    Comment,
    HeaderCrc { bytes: [u8; 2], filled: usize },
    Finished,
}

impl CompressionCodec {
    pub(in crate::context_bootstrap) fn new(format: &str, decompress: bool) -> io::Result<Self> {
        let state =
            match (format, decompress) {
                ("deflate", false) => {
                    CodecState::FlateEncoder(FlateEncoder::new(FlateFormat::Deflate))
                }
                ("deflate-raw", false) => {
                    CodecState::FlateEncoder(FlateEncoder::new(FlateFormat::DeflateRaw))
                }
                ("gzip", false) => CodecState::FlateEncoder(FlateEncoder::new(FlateFormat::Gzip)),
                ("brotli", false) => CodecState::BrotliEncoder(Some(Box::new(
                    CompressorWriter::new(Vec::new(), OUTPUT_BUFFER_SIZE, 6, 22),
                ))),
                ("deflate", true) => CodecState::FlateDecoder(FlateDecoder::new(true)),
                ("deflate-raw", true) => CodecState::FlateDecoder(FlateDecoder::new(false)),
                ("gzip", true) => CodecState::GzipDecoder(GzipDecoder::new()),
                ("brotli", true) => CodecState::BrotliDecoder(Some(Box::new(
                    DecompressorWriter::new(Vec::new(), OUTPUT_BUFFER_SIZE),
                ))),
                _ => return Err(invalid_data("unsupported compression format")),
            };
        Ok(Self { state })
    }

    pub(in crate::context_bootstrap) fn process(
        &mut self,
        input: &[u8],
        finish: bool,
    ) -> io::Result<Vec<u8>> {
        match &mut self.state {
            CodecState::FlateEncoder(encoder) => encoder.process(input, finish),
            CodecState::FlateDecoder(decoder) => decoder.process(input, finish),
            CodecState::GzipDecoder(decoder) => decoder.process(input, finish),
            CodecState::BrotliEncoder(writer) => process_brotli_encoder(writer, input, finish),
            CodecState::BrotliDecoder(writer) => process_brotli_decoder(writer, input, finish),
        }
    }
}

impl FlateEncoder {
    fn new(format: FlateFormat) -> Self {
        Self {
            compressor: Compress::new(
                Compression::default(),
                matches!(format, FlateFormat::Deflate),
            ),
            format,
            gzip_crc: Crc::new(),
            gzip_header_written: false,
            finished: false,
        }
    }

    fn process(&mut self, input: &[u8], finish: bool) -> io::Result<Vec<u8>> {
        if self.finished {
            return Err(invalid_data("compression stream is already finished"));
        }

        let mut output = Vec::new();
        if matches!(self.format, FlateFormat::Gzip) && !self.gzip_header_written {
            output.extend_from_slice(&GZIP_HEADER);
            self.gzip_header_written = true;
        }
        if matches!(self.format, FlateFormat::Gzip) {
            self.gzip_crc.update(input);
        }

        let status = drive_compressor(&mut self.compressor, input, finish, &mut output)?;
        if finish {
            if status != Status::StreamEnd {
                return Err(invalid_data("compressor did not finish"));
            }
            if matches!(self.format, FlateFormat::Gzip) {
                output.extend_from_slice(&self.gzip_crc.sum().to_le_bytes());
                output.extend_from_slice(&self.gzip_crc.amount().to_le_bytes());
            }
            self.finished = true;
        }
        Ok(output)
    }
}

impl FlateDecoder {
    fn new(zlib_header: bool) -> Self {
        Self {
            decompressor: Decompress::new(zlib_header),
            finished: false,
        }
    }

    fn process(&mut self, input: &[u8], finish: bool) -> io::Result<Vec<u8>> {
        if self.finished {
            if input.is_empty() {
                return Ok(Vec::new());
            }
            return Err(invalid_data(
                "data follows the end of the compressed stream",
            ));
        }

        let mut output = Vec::new();
        let (status, consumed) =
            drive_decompressor(&mut self.decompressor, input, finish, &mut output)?;
        if status == Status::StreamEnd {
            if consumed != input.len() {
                return Err(invalid_data(
                    "data follows the end of the compressed stream",
                ));
            }
            self.finished = true;
        } else if finish {
            return Err(unexpected_eof());
        } else if consumed != input.len() {
            return Err(invalid_data("decompressor did not consume its input"));
        }
        Ok(output)
    }
}

impl GzipDecoder {
    fn new() -> Self {
        Self {
            phase: GzipPhase::Header(GzipHeaderParser::new()),
        }
    }

    fn process(&mut self, input: &[u8], finish: bool) -> io::Result<Vec<u8>> {
        if matches!(self.phase, GzipPhase::Finished) {
            if input.is_empty() {
                return Ok(Vec::new());
            }
            return Err(invalid_data("data follows the end of the gzip member"));
        }

        let mut output = Vec::new();
        let mut offset = 0;
        loop {
            match &mut self.phase {
                GzipPhase::Header(parser) => {
                    offset += parser.consume(&input[offset..])?;
                    if !parser.is_finished() {
                        break;
                    }
                    self.phase = GzipPhase::Body {
                        decompressor: Decompress::new(false),
                        crc: Crc::new(),
                    };
                }
                GzipPhase::Body { decompressor, crc } => {
                    let output_start = output.len();
                    let (status, consumed) =
                        drive_decompressor(decompressor, &input[offset..], false, &mut output)?;
                    offset += consumed;
                    crc.update(&output[output_start..]);
                    if status != Status::StreamEnd {
                        break;
                    }
                    self.phase = GzipPhase::Trailer {
                        crc: std::mem::replace(crc, Crc::new()),
                        bytes: [0; 8],
                        filled: 0,
                    };
                }
                GzipPhase::Trailer { crc, bytes, filled } => {
                    let count = (bytes.len() - *filled).min(input.len() - offset);
                    bytes[*filled..*filled + count].copy_from_slice(&input[offset..offset + count]);
                    *filled += count;
                    offset += count;
                    if *filled != bytes.len() {
                        break;
                    }
                    let expected_crc = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                    let expected_size = u32::from_le_bytes(bytes[4..].try_into().unwrap());
                    if expected_crc != crc.sum() || expected_size != crc.amount() {
                        return Err(invalid_data("gzip checksum or size does not match"));
                    }
                    self.phase = GzipPhase::Finished;
                    if offset != input.len() {
                        return Err(invalid_data("data follows the end of the gzip member"));
                    }
                    break;
                }
                GzipPhase::Finished => unreachable!(),
            }

            if offset == input.len() {
                break;
            }
        }

        if finish && !matches!(self.phase, GzipPhase::Finished) {
            return Err(unexpected_eof());
        }
        Ok(output)
    }
}

impl GzipHeaderParser {
    fn new() -> Self {
        Self {
            stage: GzipHeaderStage::Fixed {
                bytes: [0; 10],
                filled: 0,
            },
            flags: 0,
            crc: Crc::new(),
        }
    }

    fn is_finished(&self) -> bool {
        matches!(self.stage, GzipHeaderStage::Finished)
    }

    fn consume(&mut self, input: &[u8]) -> io::Result<usize> {
        let mut offset = 0;
        while offset < input.len() && !self.is_finished() {
            match &mut self.stage {
                GzipHeaderStage::Fixed { bytes, filled } => {
                    let count = (bytes.len() - *filled).min(input.len() - offset);
                    bytes[*filled..*filled + count].copy_from_slice(&input[offset..offset + count]);
                    *filled += count;
                    offset += count;
                    if *filled == bytes.len() {
                        if bytes[0..3] != [0x1f, 0x8b, 8] || bytes[3] & 0xe0 != 0 {
                            return Err(invalid_data("invalid gzip header"));
                        }
                        self.flags = bytes[3];
                        self.crc.update(bytes);
                        self.stage = Self::stage_after_fixed(self.flags);
                    }
                }
                GzipHeaderStage::ExtraLength { bytes, filled } => {
                    let count = (bytes.len() - *filled).min(input.len() - offset);
                    let chunk = &input[offset..offset + count];
                    bytes[*filled..*filled + count].copy_from_slice(chunk);
                    self.crc.update(chunk);
                    *filled += count;
                    offset += count;
                    if *filled == bytes.len() {
                        self.stage = GzipHeaderStage::Extra {
                            remaining: u16::from_le_bytes(*bytes) as usize,
                        };
                    }
                }
                GzipHeaderStage::Extra { remaining } => {
                    let count = (*remaining).min(input.len() - offset);
                    self.crc.update(&input[offset..offset + count]);
                    *remaining -= count;
                    offset += count;
                    if *remaining == 0 {
                        self.stage = Self::stage_after_extra(self.flags);
                    }
                }
                GzipHeaderStage::Name => {
                    let consumed = self.consume_zero_terminated(&input[offset..]);
                    offset += consumed;
                    if input[offset - consumed..offset].last() == Some(&0) {
                        self.stage = Self::stage_after_name(self.flags);
                    }
                }
                GzipHeaderStage::Comment => {
                    let consumed = self.consume_zero_terminated(&input[offset..]);
                    offset += consumed;
                    if input[offset - consumed..offset].last() == Some(&0) {
                        self.stage = Self::stage_after_comment(self.flags);
                    }
                }
                GzipHeaderStage::HeaderCrc { bytes, filled } => {
                    let count = (bytes.len() - *filled).min(input.len() - offset);
                    bytes[*filled..*filled + count].copy_from_slice(&input[offset..offset + count]);
                    *filled += count;
                    offset += count;
                    if *filled == bytes.len() {
                        if u16::from_le_bytes(*bytes) != self.crc.sum() as u16 {
                            return Err(invalid_data("gzip header checksum does not match"));
                        }
                        self.stage = GzipHeaderStage::Finished;
                    }
                }
                GzipHeaderStage::Finished => unreachable!(),
            }
        }
        Ok(offset)
    }

    fn consume_zero_terminated(&mut self, input: &[u8]) -> usize {
        let count = input
            .iter()
            .position(|byte| *byte == 0)
            .map_or(input.len(), |position| position + 1);
        self.crc.update(&input[..count]);
        count
    }

    fn stage_after_fixed(flags: u8) -> GzipHeaderStage {
        if flags & 0x04 != 0 {
            GzipHeaderStage::ExtraLength {
                bytes: [0; 2],
                filled: 0,
            }
        } else {
            Self::stage_after_extra(flags)
        }
    }

    fn stage_after_extra(flags: u8) -> GzipHeaderStage {
        if flags & 0x08 != 0 {
            GzipHeaderStage::Name
        } else {
            Self::stage_after_name(flags)
        }
    }

    fn stage_after_name(flags: u8) -> GzipHeaderStage {
        if flags & 0x10 != 0 {
            GzipHeaderStage::Comment
        } else {
            Self::stage_after_comment(flags)
        }
    }

    fn stage_after_comment(flags: u8) -> GzipHeaderStage {
        if flags & 0x02 != 0 {
            GzipHeaderStage::HeaderCrc {
                bytes: [0; 2],
                filled: 0,
            }
        } else {
            GzipHeaderStage::Finished
        }
    }
}

fn drive_compressor(
    compressor: &mut Compress,
    input: &[u8],
    finish: bool,
    output: &mut Vec<u8>,
) -> io::Result<Status> {
    let mut consumed = 0;
    loop {
        output.reserve(OUTPUT_BUFFER_SIZE);
        let available_output = output.capacity() - output.len();
        let input_before = compressor.total_in();
        let output_before = compressor.total_out();
        let status = compressor
            .compress_vec(
                &input[consumed..],
                output,
                if finish {
                    FlushCompress::Finish
                } else {
                    FlushCompress::None
                },
            )
            .map_err(|error| invalid_data(error.to_string()))?;
        let input_delta = (compressor.total_in() - input_before) as usize;
        let output_delta = (compressor.total_out() - output_before) as usize;
        consumed += input_delta;

        if status == Status::StreamEnd {
            if consumed != input.len() {
                return Err(invalid_data("compressor did not consume its input"));
            }
            return Ok(status);
        }
        if consumed == input.len() && (!finish || output_delta < available_output) {
            return Ok(status);
        }
        if input_delta == 0 && output_delta == 0 {
            return Err(invalid_data("compressor made no progress"));
        }
    }
}

fn drive_decompressor(
    decompressor: &mut Decompress,
    input: &[u8],
    finish: bool,
    output: &mut Vec<u8>,
) -> io::Result<(Status, usize)> {
    let mut consumed = 0;
    loop {
        output.reserve(OUTPUT_BUFFER_SIZE);
        let available_output = output.capacity() - output.len();
        let input_before = decompressor.total_in();
        let output_before = decompressor.total_out();
        let status = decompressor
            .decompress_vec(
                &input[consumed..],
                output,
                if finish {
                    FlushDecompress::Finish
                } else {
                    FlushDecompress::None
                },
            )
            .map_err(|error| invalid_data(error.to_string()))?;
        let input_delta = (decompressor.total_in() - input_before) as usize;
        let output_delta = (decompressor.total_out() - output_before) as usize;
        consumed += input_delta;

        if status == Status::StreamEnd {
            return Ok((status, consumed));
        }
        if consumed == input.len() && output_delta < available_output {
            return Ok((status, consumed));
        }
        if input_delta == 0 && output_delta == 0 {
            return Ok((status, consumed));
        }
    }
}

fn process_brotli_encoder(
    writer: &mut Option<Box<CompressorWriter<Vec<u8>>>>,
    input: &[u8],
    finish: bool,
) -> io::Result<Vec<u8>> {
    let active = writer
        .as_mut()
        .ok_or_else(|| invalid_data("compression stream is already finished"))?;
    active.write_all(input)?;
    if finish {
        return Ok(writer.take().unwrap().into_inner());
    }
    Ok(std::mem::take(active.get_mut()))
}

fn process_brotli_decoder(
    writer: &mut Option<Box<DecompressorWriter<Vec<u8>>>>,
    input: &[u8],
    finish: bool,
) -> io::Result<Vec<u8>> {
    let active = writer
        .as_mut()
        .ok_or_else(|| invalid_data("data follows the end of the brotli stream"))?;
    if !input.is_empty() {
        let consumed = active.write(input)?;
        if consumed != input.len() {
            return Err(invalid_data("data follows the end of the brotli stream"));
        }
    }
    if finish {
        return writer
            .take()
            .unwrap()
            .into_inner()
            .map_err(|_| unexpected_eof());
    }
    Ok(std::mem::take(active.get_mut()))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.into())
}

fn unexpected_eof() -> io::Error {
    io::Error::new(
        ErrorKind::UnexpectedEof,
        "compressed stream ended before its end marker",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(codec: &mut CompressionCodec) -> io::Result<Vec<u8>> {
        codec.process(&[], true)
    }

    #[test]
    fn codecs_preserve_data_across_single_byte_boundaries() {
        let input: Vec<u8> = (0..512).map(|index| (index % 251) as u8).collect();
        for format in ["gzip", "deflate", "deflate-raw", "brotli"] {
            let mut encoder = CompressionCodec::new(format, false).unwrap();
            let mut encoded = Vec::new();
            for chunk in input.chunks(7) {
                encoded.extend(encoder.process(chunk, false).unwrap());
            }
            encoded.extend(finish(&mut encoder).unwrap());

            let mut decoder = CompressionCodec::new(format, true).unwrap();
            let mut decoded = Vec::new();
            for byte in &encoded {
                decoded.extend(decoder.process(std::slice::from_ref(byte), false).unwrap());
            }
            decoded.extend(finish(&mut decoder).unwrap());
            assert_eq!(decoded, input, "{format}");
        }
    }

    #[test]
    fn codecs_drain_output_larger_than_the_scratch_buffer() {
        let input: Vec<u8> = (0..65_537).map(|index| (index % 251) as u8).collect();
        for format in ["gzip", "deflate", "deflate-raw", "brotli"] {
            let mut encoder = CompressionCodec::new(format, false).unwrap();
            let mut encoded = encoder.process(&input, false).unwrap();
            encoded.extend(finish(&mut encoder).unwrap());
            let mut decoder = CompressionCodec::new(format, true).unwrap();
            let mut decoded = decoder.process(&encoded, false).unwrap();
            decoded.extend(finish(&mut decoder).unwrap());
            assert_eq!(decoded, input, "{format}");
        }
    }

    fn gzip_with_header_crc(contents: &[u8]) -> Vec<u8> {
        let mut encoder = CompressionCodec::new("gzip", false).unwrap();
        let mut encoded = encoder.process(contents, false).unwrap();
        encoded.extend(finish(&mut encoder).unwrap());
        encoded[3] |= 0x02;
        let mut crc = Crc::new();
        crc.update(&encoded[..10]);
        encoded.splice(10..10, (crc.sum() as u16).to_le_bytes());
        encoded
    }

    #[test]
    fn gzip_validates_header_crc_and_rejects_trailing_members() {
        let encoded = gzip_with_header_crc(b"header crc");
        let mut decoder = CompressionCodec::new("gzip", true).unwrap();
        assert_eq!(decoder.process(&encoded, false).unwrap(), b"header crc");
        assert!(finish(&mut decoder).is_ok());

        let mut corrupt = encoded.clone();
        corrupt[10] ^= 1;
        let mut decoder = CompressionCodec::new("gzip", true).unwrap();
        assert_eq!(
            decoder.process(&corrupt, false).unwrap_err().kind(),
            ErrorKind::InvalidData
        );

        let mut concatenated = encoded.clone();
        concatenated.extend_from_slice(&encoded);
        let mut decoder = CompressionCodec::new("gzip", true).unwrap();
        assert_eq!(
            decoder.process(&concatenated, false).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn zlib_rejects_dictionary_streams_and_bad_checksums() {
        let dictionary_stream = [0x78, 0x20, 0, 0, 0, 0];
        let mut decoder = CompressionCodec::new("deflate", true).unwrap();
        assert_eq!(
            decoder
                .process(&dictionary_stream, false)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );

        let mut encoder = CompressionCodec::new("deflate", false).unwrap();
        let mut encoded = encoder.process(b"checksum", false).unwrap();
        encoded.extend(finish(&mut encoder).unwrap());
        *encoded.last_mut().unwrap() ^= 1;
        let mut decoder = CompressionCodec::new("deflate", true).unwrap();
        assert_eq!(
            decoder.process(&encoded, false).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn brotli_rejects_truncation_and_trailing_bytes() {
        let mut encoder = CompressionCodec::new("brotli", false).unwrap();
        let mut encoded = encoder.process(b"brotli end marker", false).unwrap();
        encoded.extend(finish(&mut encoder).unwrap());

        let mut truncated = CompressionCodec::new("brotli", true).unwrap();
        truncated
            .process(&encoded[..encoded.len() - 1], false)
            .unwrap();
        assert_eq!(
            finish(&mut truncated).unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );

        encoded.push(0);
        let mut trailing = CompressionCodec::new("brotli", true).unwrap();
        assert_eq!(
            trailing.process(&encoded, false).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
}
