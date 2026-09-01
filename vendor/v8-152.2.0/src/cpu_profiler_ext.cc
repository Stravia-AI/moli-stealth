// Copyright 2026 the Moli authors. MIT license.

#include "v8-profiler.h"
#include "v8.h"

#include <cstddef>
#include <cstdint>

namespace {

using WriteCallback = bool (*)(void* context,
                               const char* data,
                               size_t size);

struct CpuTraceProfilerState {
  v8::Isolate* isolate;
  v8::CpuProfiler* profiler;
  unsigned max_samples;
};

class CallbackOutputStream final : public v8::OutputStream {
 public:
  CallbackOutputStream(void* context, WriteCallback callback)
      : context_(context), callback_(callback) {}

  void EndOfStream() override { ended_ = true; }

  int GetChunkSize() override { return 64 * 1024; }

  WriteResult WriteAsciiChunk(char* data, int size) override {
    if (size < 0 || !callback_(context_, data, static_cast<size_t>(size))) {
      return kAbort;
    }
    return kContinue;
  }

  bool ended() const { return ended_; }

 private:
  void* context_;
  WriteCallback callback_;
  bool ended_ = false;
};

void DisposeProfilerState(CpuTraceProfilerState* state) {
  state->profiler->Dispose();
  delete state;
}

}  // namespace

extern "C" {

void* v8__CpuTraceProfiler__Start(v8::Isolate* isolate,
                                  int sampling_interval_us,
                                  unsigned max_samples) {
  if (!isolate || v8::Isolate::GetCurrent() != isolate ||
      sampling_interval_us <= 0 || max_samples == 0) {
    return nullptr;
  }

  v8::HandleScope handle_scope(isolate);
  auto* profiler = v8::CpuProfiler::New(
      isolate, v8::CpuProfilingNamingMode::kDebugNaming,
      v8::CpuProfilingLoggingMode::kLazyLogging);
  if (!profiler) {
    return nullptr;
  }
  profiler->SetSamplingInterval(sampling_interval_us);
  const auto status = profiler->StartProfiling(
      v8::String::Empty(isolate), v8::CpuProfilingMode::kLeafNodeLineNumbers,
      true, max_samples);
  if (status != v8::CpuProfilingStatus::kStarted) {
    profiler->Dispose();
    return nullptr;
  }
  return new CpuTraceProfilerState{isolate, profiler, max_samples};
}

bool v8__CpuTraceProfiler__Stop(void* raw_state,
                               void* callback_context,
                               WriteCallback callback,
                               unsigned* sample_count,
                               bool* sample_limit_reached) {
  auto* state = static_cast<CpuTraceProfilerState*>(raw_state);
  if (!state || !callback || !sample_count || !sample_limit_reached ||
      v8::Isolate::GetCurrent() != state->isolate) {
    return false;
  }

  v8::HandleScope handle_scope(state->isolate);
  auto* profile =
      state->profiler->StopProfiling(v8::String::Empty(state->isolate));
  if (!profile) {
    DisposeProfilerState(state);
    return false;
  }

  const int recorded_samples = profile->GetSamplesCount();
  *sample_count = recorded_samples > 0
                      ? static_cast<unsigned>(recorded_samples)
                      : 0;
  *sample_limit_reached = *sample_count >= state->max_samples;

  CallbackOutputStream stream(callback_context, callback);
  profile->Serialize(&stream, v8::CpuProfile::SerializationFormat::kJSON);
  profile->Delete();
  DisposeProfilerState(state);
  return stream.ended();
}

bool v8__CpuTraceProfiler__Cancel(void* raw_state) {
  auto* state = static_cast<CpuTraceProfilerState*>(raw_state);
  if (!state || v8::Isolate::GetCurrent() != state->isolate) {
    return false;
  }

  v8::HandleScope handle_scope(state->isolate);
  if (auto* profile =
          state->profiler->StopProfiling(v8::String::Empty(state->isolate))) {
    profile->Delete();
  }
  DisposeProfilerState(state);
  return true;
}

}  // extern "C"
