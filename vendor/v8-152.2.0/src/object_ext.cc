// Copyright 2026 the Moli authors. MIT license.

#include "support.h"
#include "v8-object.h"

using namespace support;

extern "C" {

MaybeBool v8__Object__SetNativeDataProperty(
    const v8::Object& self, const v8::Context& context,
    const v8::Name& key, v8::AccessorNameGetterCallback getter,
    v8::AccessorNameSetterCallback setter, const v8::Value* data_or_null,
    v8::PropertyAttribute attr) {
  return maybe_to_maybe_bool(ptr_to_local(&self)->SetNativeDataProperty(
      ptr_to_local(&context), ptr_to_local(&key), getter, setter,
      ptr_to_local(data_or_null), attr));
}

}  // extern "C"
