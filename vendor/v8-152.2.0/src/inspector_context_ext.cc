// Copyright 2026 the Moli authors. MIT license.

#include "support.h"
#include "v8-inspector.h"

#include <algorithm>
#include <cstdint>
#include <vector>

using namespace support;

namespace v8::debug {
MaybeLocal<UnboundScript> CompileInspectorScript(Isolate* isolate,
                                                 Local<String> source);
}

extern "C" {

void v8_inspector__V8Inspector__contextCreatedWithOrigin(
    v8_inspector::V8Inspector* inspector, const v8::Context& context,
    int context_group_id, v8_inspector::StringView human_readable_name,
    v8_inspector::StringView origin, v8_inspector::StringView aux_data) {
  v8_inspector::V8ContextInfo context_info(
      ptr_to_local(&context), context_group_id, human_readable_name);
  context_info.origin = origin;
  context_info.auxData = aux_data;
  inspector->contextCreated(context_info);
}

int v8_inspector__V8ContextInfo__executionContextId(
    const v8::Context& context) {
  return v8_inspector::V8ContextInfo::executionContextId(
      ptr_to_local(&context));
}

void v8_inspector__V8Inspector__resetContextGroup(
    v8_inspector::V8Inspector* inspector, int context_group_id) {
  inspector->resetContextGroup(context_group_id);
}

std::vector<uint8_t>* v8_inspector__V8InspectorSession__state(
    v8_inspector::V8InspectorSession* session) {
  return new std::vector<uint8_t>(session->state());
}

void v8_inspector__V8InspectorSession__State__DELETE(
    std::vector<uint8_t>* state) {
  delete state;
}

size_t v8_inspector__V8InspectorSession__State__size(
    const std::vector<uint8_t>* state) {
  return state->size();
}

void v8_inspector__V8InspectorSession__State__copy(
    const std::vector<uint8_t>* state, uint8_t* out) {
  std::copy(state->begin(), state->end(), out);
}

const v8::UnboundScript* v8_inspector__CompileInspectorScript(
    v8::Isolate* isolate, const v8::String* source) {
  return maybe_local_to_ptr(
      v8::debug::CompileInspectorScript(isolate, ptr_to_local(source)));
}

}  // extern "C"
