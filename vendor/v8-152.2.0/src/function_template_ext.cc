// Copyright 2026 the Moli authors. MIT license.

#include "support.h"
#include "v8-template.h"

using namespace support;

extern "C" {

bool v8__FunctionTemplate__HasInstance(const v8::FunctionTemplate& self,
                                       const v8::Value& value) {
  return ptr_to_local(&self)->HasInstance(ptr_to_local(&value));
}

}  // extern "C"
