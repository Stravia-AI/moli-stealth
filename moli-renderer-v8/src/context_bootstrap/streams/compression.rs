use std::{cell::RefCell, ffi::c_void, io, rc::Rc};

use super::compression_codec::CompressionCodec;
use crate::util::{get_private_value, set_private_value};

const COMPRESSION_STREAM_CODEC_SLOT: &str = "__moliCompressionStreamCodec";
pub(super) const COMPRESSION_STREAM_BRAND_SLOT: &str = "__moliCompressionStreamBrand";

struct CompressionStreamCodecState {
    codec: Option<CompressionCodec>,
}

pub(in crate::context_bootstrap) fn install_compression_stream_codec<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    writable: v8::Local<'s, v8::Object>,
    codec: CompressionCodec,
) {
    let state = Rc::new(RefCell::new(CompressionStreamCodecState {
        codec: Some(codec),
    }));
    let pointer = Rc::as_ptr(&state) as *mut c_void;
    set_private_value(
        scope,
        writable,
        COMPRESSION_STREAM_CODEC_SLOT,
        v8::External::new(scope, pointer).into(),
    );
    crate::v8_finalizer::track_context_owned_v8_finalizer(scope, writable, move || drop(state));
}

pub(in crate::context_bootstrap) fn process_compression_stream_codec<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    writable: v8::Local<'s, v8::Object>,
    input: &[u8],
    finish: bool,
) -> io::Result<Vec<u8>> {
    let pointer = compression_stream_codec_state_pointer(scope, writable)?;
    // The writable endpoint owns an Rc through its V8 finalizer. Native stream
    // algorithms can only reach this slot while that endpoint is live.
    let state = unsafe { &*pointer };
    let mut state = state
        .try_borrow_mut()
        .map_err(|_| io::Error::other("CompressionStream codec is already processing a chunk"))?;
    let result = state
        .codec
        .as_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "CompressionStream is closed"))?
        .process(input, finish);
    if finish || result.is_err() {
        state.codec.take();
    }
    result
}

pub(in crate::context_bootstrap) fn discard_compression_stream_codec<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    writable: v8::Local<'s, v8::Object>,
) {
    if let Ok(pointer) = compression_stream_codec_state_pointer(scope, writable) {
        // The writable endpoint retains the owning Rc until its finalizer runs.
        let state = unsafe { &*pointer };
        if let Ok(mut state) = state.try_borrow_mut() {
            state.codec.take();
        }
    }
}

fn compression_stream_codec_state_pointer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    writable: v8::Local<'s, v8::Object>,
) -> io::Result<*const RefCell<CompressionStreamCodecState>> {
    let external = get_private_value(scope, writable, COMPRESSION_STREAM_CODEC_SLOT)
        .and_then(|value| v8::Local::<v8::External>::try_from(value).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "CompressionStream codec state is unavailable",
            )
        })?;
    let pointer = external
        .value()
        .cast::<RefCell<CompressionStreamCodecState>>();
    if pointer.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "CompressionStream codec state is unavailable",
        ));
    }
    Ok(pointer)
}
