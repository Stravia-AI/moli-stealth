// Copyright 2026 the Moli authors. MIT license.

#include "support.h"
#include "v8-context.h"
#include "v8-isolate.h"

using namespace support;

static_assert(sizeof(v8::Context::BackupIncumbentScope) ==
                  sizeof(size_t) * 3,
              "BackupIncumbentScope size mismatch");

extern "C" {

void v8__Context__BackupIncumbentScope__CONSTRUCT(
    uninit_t<v8::Context::BackupIncumbentScope>* buf,
    const v8::Context& context) {
  construct_in_place<v8::Context::BackupIncumbentScope>(buf,
                                                        ptr_to_local(&context));
}

void v8__Context__BackupIncumbentScope__DESTRUCT(
    v8::Context::BackupIncumbentScope* self) {
  self->~BackupIncumbentScope();
}

void v8__Context__DetachGlobal(const v8::Context& self) {
  ptr_to_local(&self)->DetachGlobal();
}

const v8::Context* v8__Isolate__GetIncumbentContext(v8::Isolate* isolate) {
  return local_to_ptr(isolate->GetIncumbentContext());
}

void v8__Isolate__SetFailedAccessCheckCallbackFunction(
    v8::Isolate* isolate, v8::FailedAccessCheckCallback callback) {
  isolate->SetFailedAccessCheckCallbackFunction(callback);
}

}  // extern "C"
