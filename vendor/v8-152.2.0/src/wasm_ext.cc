// Copyright 2026 the Moli authors. MIT license.

#include <cstring>

#include "support.h"
#include "v8.h"

#if __has_include("v8/src/api/api-inl.h") && \
    __has_include("v8/src/wasm/wasm-engine.h")
#define MOLI_HAS_V8_INTERNAL_WASM_COMPILE 1
#include "v8/src/api/api-inl.h"
#include "v8/src/base/vector.h"
#include "v8/src/wasm/wasm-engine.h"
#include "v8/src/wasm/wasm-features.h"
#include "v8/src/wasm/wasm-result.h"
#else
#define MOLI_HAS_V8_INTERNAL_WASM_COMPILE 0
#endif

using namespace support;

namespace {

#if !MOLI_HAS_V8_INTERNAL_WASM_COMPILE
v8::MaybeLocal<v8::String> NewUtf8Literal(v8::Isolate* isolate,
                                          const char* literal) {
  return v8::String::NewFromUtf8(isolate, literal,
                                 v8::NewStringType::kInternalized);
}

v8::MaybeLocal<v8::Value> GetProperty(v8::Isolate* isolate,
                                      v8::Local<v8::Context> context,
                                      v8::Local<v8::Object> object,
                                      const char* name) {
  v8::Local<v8::String> key;
  if (!NewUtf8Literal(isolate, name).ToLocal(&key)) {
    return v8::MaybeLocal<v8::Value>();
  }
  return object->Get(context, key);
}

v8::MaybeLocal<v8::WasmModuleObject> RethrowCaughtWasmCompileException(
    v8::TryCatch* try_catch) {
  if (try_catch->HasCaught()) {
    try_catch->ReThrow();
  }
  return v8::MaybeLocal<v8::WasmModuleObject>();
}

v8::MaybeLocal<v8::WasmModuleObject> CompileViaIsolatedPublicConstructor(
    v8::Isolate* isolate, v8::MemorySpan<const uint8_t> wire_bytes,
    bool js_string_builtins, const char* string_constants_module_data,
    size_t string_constants_module_len) {
  bool has_string_constants = string_constants_module_data != nullptr;

  v8::EscapableHandleScope handle_scope(isolate);
  v8::TryCatch try_catch(isolate);
  v8::Local<v8::Context> context = v8::Context::New(isolate);
  v8::Context::Scope context_scope(context);

  v8::Local<v8::Value> webassembly_value;
  if (!GetProperty(isolate, context, context->Global(), "WebAssembly")
           .ToLocal(&webassembly_value) ||
      !webassembly_value->IsObject()) {
    return RethrowCaughtWasmCompileException(&try_catch);
  }

  v8::Local<v8::Value> module_value;
  if (!GetProperty(isolate, context, webassembly_value.As<v8::Object>(),
                   "Module")
           .ToLocal(&module_value) ||
      !module_value->IsFunction()) {
    return RethrowCaughtWasmCompileException(&try_catch);
  }

  v8::Local<v8::ArrayBuffer> buffer = v8::ArrayBuffer::New(
      isolate, wire_bytes.size(),
      v8::BackingStoreInitializationMode::kUninitialized);
  if (wire_bytes.size() != 0) {
    std::memcpy(buffer->Data(), wire_bytes.data(), wire_bytes.size());
  }

  v8::Local<v8::Uint8Array> bytes =
      v8::Uint8Array::New(buffer, 0, wire_bytes.size());
  v8::Local<v8::Object> options = v8::Object::New(isolate);
  if (js_string_builtins) {
    v8::Local<v8::Array> builtins = v8::Array::New(isolate, 1);
    v8::Local<v8::String> js_string;
    v8::Local<v8::String> builtins_key;
    if (!NewUtf8Literal(isolate, "js-string").ToLocal(&js_string) ||
        !NewUtf8Literal(isolate, "builtins").ToLocal(&builtins_key) ||
        !builtins->Set(context, 0, js_string).FromMaybe(false) ||
        !options->Set(context, builtins_key, builtins).FromMaybe(false)) {
      return RethrowCaughtWasmCompileException(&try_catch);
    }
  }

  if (has_string_constants) {
    v8::Local<v8::String> constants_module;
    v8::Local<v8::String> constants_key;
    if (!v8::String::NewFromUtf8(
             isolate, string_constants_module_data, v8::NewStringType::kNormal,
             static_cast<int>(string_constants_module_len))
             .ToLocal(&constants_module) ||
        !NewUtf8Literal(isolate, "importedStringConstants")
             .ToLocal(&constants_key) ||
        !options->Set(context, constants_key, constants_module)
             .FromMaybe(false)) {
      return RethrowCaughtWasmCompileException(&try_catch);
    }
  }

  v8::Local<v8::Value> args[] = {bytes, options};
  v8::Local<v8::Object> module;
  if (!module_value.As<v8::Function>()
           ->NewInstance(context, std::size(args), args)
           .ToLocal(&module) ||
      !module->IsWasmModuleObject()) {
    return RethrowCaughtWasmCompileException(&try_catch);
  }

  return handle_scope.Escape(module.As<v8::WasmModuleObject>());
}
#endif  // !MOLI_HAS_V8_INTERNAL_WASM_COMPILE

v8::MaybeLocal<v8::WasmModuleObject> CompileWithOptions(
    v8::Isolate* v8_isolate, v8::MemorySpan<const uint8_t> wire_bytes,
    bool js_string_builtins, const char* string_constants_module_data,
    size_t string_constants_module_len) {
#if MOLI_HAS_V8_INTERNAL_WASM_COMPILE
#if V8_ENABLE_WEBASSEMBLY
  // Source builds can use the same internal path as V8's public API and pass
  // compile-time imports directly. Prebuilt rusty_v8 only ships public headers,
  // so that configuration falls back to the public constructor path below.
  v8::base::OwnedVector<const uint8_t> bytes =
      v8::base::OwnedCopyOf(wire_bytes);
  v8::internal::Isolate* isolate =
      reinterpret_cast<v8::internal::Isolate*>(v8_isolate);
  v8::internal::wasm::CompileTimeImports compile_imports;
  if (js_string_builtins) {
    compile_imports.Add(v8::internal::wasm::CompileTimeImport::kJsString);
  }
  if (string_constants_module_data != nullptr) {
    compile_imports.Add(
        v8::internal::wasm::CompileTimeImport::kStringConstants);
    compile_imports.constants_module().assign(string_constants_module_data,
                                              string_constants_module_len);
  }
  v8::internal::wasm::ErrorThrower thrower(
      isolate, "WasmModuleObject::CompileWithOptions()");
  auto enabled_features =
      v8::internal::wasm::WasmEnabledFeatures::FromIsolate(isolate);
  v8::internal::MaybeDirectHandle<v8::internal::WasmModuleObject>
      maybe_compiled = v8::internal::wasm::GetWasmEngine()->SyncCompile(
          isolate, enabled_features, std::move(compile_imports), &thrower,
          std::move(bytes));
  if (maybe_compiled.is_null()) {
    return v8::MaybeLocal<v8::WasmModuleObject>();
  }
  return v8::Utils::ToLocal(maybe_compiled.ToHandleChecked());
#else
  v8::Utils::ApiCheck(false, "WasmModuleObject::CompileWithOptions",
                      "WebAssembly support is not enabled");
  return v8::MaybeLocal<v8::WasmModuleObject>();
#endif
#else
  // The public C++ API has no compile-options overload yet. Use a fresh V8
  // context so the bridge exercises V8's built-in WebAssembly.Module
  // constructor without observing or trusting the page's mutable global object.
  return CompileViaIsolatedPublicConstructor(v8_isolate, wire_bytes,
                                             js_string_builtins,
                                             string_constants_module_data,
                                             string_constants_module_len);
#endif  // MOLI_HAS_V8_INTERNAL_WASM_COMPILE
}

}  // namespace

extern "C" {

const v8::WasmModuleObject* v8__WasmModuleObject__CompileWithOptions(
    v8::Isolate* isolate, const uint8_t* wire_bytes_data, size_t length,
    bool js_string_builtins, const char* string_constants_module_data,
    size_t string_constants_module_len) {
  v8::MemorySpan<const uint8_t> wire_bytes(wire_bytes_data, length);
  return maybe_local_to_ptr(
      CompileWithOptions(isolate, wire_bytes, js_string_builtins,
                         string_constants_module_data,
                         string_constants_module_len));
}

}  // extern "C"
