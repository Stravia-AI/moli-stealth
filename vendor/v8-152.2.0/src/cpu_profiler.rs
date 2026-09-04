use crate::isolate::{RealIsolate, UnsafeRawIsolatePtr};
use std::ffi::c_void;
use std::ptr::NonNull;

type WriteCallback = unsafe extern "C" fn(*mut c_void, *const u8, usize) -> bool;

unsafe extern "C" {
  fn v8__CpuTraceProfiler__Start(
    isolate: *mut RealIsolate,
    sampling_interval_us: i32,
    max_samples: u32,
  ) -> *mut c_void;
  fn v8__CpuTraceProfiler__Stop(
    profiler: *mut c_void,
    callback_context: *mut c_void,
    callback: WriteCallback,
    sample_count: *mut u32,
    sample_limit_reached: *mut bool,
  ) -> bool;
  fn v8__CpuTraceProfiler__Cancel(profiler: *mut c_void) -> bool;
}

/// Opaque owner-thread handle for one internal V8 CPU profile.
///
/// The token may be transferred through a coordinator, but it must only be
/// stopped or cancelled while its exact isolate is entered on the isolate
/// owner thread. Dropping an unconsumed token leaks the native profiler, so
/// embedders must preserve that lifecycle invariant through isolate teardown.
#[derive(Debug)]
#[must_use = "a CPU trace profiler must be stopped or cancelled on its isolate owner"]
pub struct CpuTraceProfiler {
  handle: NonNull<c_void>,
}

// The pointer is only an opaque transport token. Access remains restricted to
// unsafe owner-thread methods, and no native profiler operation happens while
// the token is in transit.
unsafe impl Send for CpuTraceProfiler {}

#[derive(Debug)]
pub struct CpuTraceProfile {
  pub bytes: Vec<u8>,
  pub sample_count: u32,
  pub sample_limit_reached: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuTraceProfilerStopError {
  InvalidOwnerOrState,
  OutputLimitExceeded,
}

impl CpuTraceProfiler {
  /// Starts profiling an entered, live isolate.
  ///
  /// # Safety
  ///
  /// `isolate` must identify the current isolate on its owner thread and must
  /// remain live until the returned token is stopped or cancelled there.
  pub unsafe fn start_on_current_isolate(
    isolate: UnsafeRawIsolatePtr,
    sampling_interval_us: i32,
    max_samples: u32,
  ) -> Option<Self> {
    // SAFETY: UnsafeRawIsolatePtr is a transparent RealIsolate pointer. The
    // caller owns the current-isolate and lifetime invariants documented above.
    let isolate: *mut RealIsolate = unsafe { std::mem::transmute(isolate) };
    let handle = unsafe {
      v8__CpuTraceProfiler__Start(
        isolate,
        sampling_interval_us,
        max_samples,
      )
    };
    NonNull::new(handle).map(|handle| Self { handle })
  }

  /// Stops and serializes this profiler on its entered isolate owner.
  ///
  /// # Safety
  ///
  /// The exact isolate used by `start_on_current_isolate` must be current on
  /// this thread. The isolate must still be live.
  pub unsafe fn stop_on_current_isolate(
    self,
    max_output_bytes: usize,
  ) -> Result<CpuTraceProfile, CpuTraceProfilerStopError> {
    struct OutputBuffer {
      bytes: Vec<u8>,
      max_bytes: usize,
      limit_exceeded: bool,
    }

    unsafe extern "C" fn append_output(
      context: *mut c_void,
      data: *const u8,
      size: usize,
    ) -> bool {
      // SAFETY: the native serializer calls this synchronously before stop
      // returns, with the OutputBuffer passed immediately below.
      let output = unsafe { &mut *context.cast::<OutputBuffer>() };
      let Some(new_len) = output.bytes.len().checked_add(size) else {
        output.limit_exceeded = true;
        return false;
      };
      if new_len > output.max_bytes || output.bytes.try_reserve(size).is_err() {
        output.limit_exceeded = true;
        return false;
      }
      // SAFETY: V8 owns a readable chunk of exactly `size` bytes for the
      // duration of this callback.
      let chunk = unsafe { std::slice::from_raw_parts(data, size) };
      output.bytes.extend_from_slice(chunk);
      true
    }

    let mut output = OutputBuffer {
      bytes: Vec::new(),
      max_bytes: max_output_bytes,
      limit_exceeded: false,
    };
    let mut sample_count = 0;
    let mut sample_limit_reached = false;
    let completed = unsafe {
      v8__CpuTraceProfiler__Stop(
        self.handle.as_ptr(),
        (&mut output as *mut OutputBuffer).cast(),
        append_output,
        &mut sample_count,
        &mut sample_limit_reached,
      )
    };
    if output.limit_exceeded {
      return Err(CpuTraceProfilerStopError::OutputLimitExceeded);
    }
    if !completed {
      return Err(CpuTraceProfilerStopError::InvalidOwnerOrState);
    }
    Ok(CpuTraceProfile {
      bytes: output.bytes,
      sample_count,
      sample_limit_reached,
    })
  }

  /// Cancels this profiler without serializing it.
  ///
  /// # Safety
  ///
  /// The exact live isolate used to start this profiler must be current on the
  /// calling owner thread.
  pub unsafe fn cancel_on_current_isolate(self) -> bool {
    unsafe { v8__CpuTraceProfiler__Cancel(self.handle.as_ptr()) }
  }
}
