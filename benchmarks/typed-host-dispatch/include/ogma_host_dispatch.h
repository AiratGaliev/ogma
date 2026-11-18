#ifndef OGMA_HOST_DISPATCH_H
#define OGMA_HOST_DISPATCH_H

#include <stdint.h>
#include <stddef.h>
#include <ogma_abi.h>

#ifdef __cplusplus
extern "C" {
#endif

#define OGMA_HOST_DESCRIPTOR_VERSION UINT16_C(1)
#define OGMA_HOST_SIGNATURE_GENERIC UINT32_C(0)
#define OGMA_HOST_SIGNATURE_F64_BINARY UINT32_C(1)
#define OGMA_HOST_SIGNATURE_NUMBER_HANDLE_BINARY UINT32_C(2)

#define OGMA_HOST_EFFECT_MAY_ALLOCATE (UINT32_C(1) << 0)
#define OGMA_HOST_EFFECT_MAY_GC       (UINT32_C(1) << 1)
#define OGMA_HOST_EFFECT_MAY_THROW    (UINT32_C(1) << 2)
#define OGMA_HOST_EFFECT_MAY_SUSPEND  (UINT32_C(1) << 3)
#define OGMA_HOST_EFFECT_MAY_REENTER  (UINT32_C(1) << 4)

typedef OgmaStatus (*OgmaPrimitiveF64Thunk)(
    const OgmaRuntime *runtime, OgmaContextHandle context,
    double left, double right, double *out_result, void *user_data);

typedef OgmaStatus (*OgmaNumberHandleF64Thunk)(
    const OgmaRuntime *runtime, OgmaContextHandle context,
    OgmaHandle left, OgmaHandle right, double *out_result, void *user_data);

typedef struct {
    uint32_t struct_size;
    uint16_t descriptor_version;
    uint16_t abi_major;
    uint16_t abi_minor;
    uint16_t reserved;
    uint64_t function_id;
    uint8_t name[128];
    uint16_t name_len;
    uint16_t reserved_name;
    uint32_t signature;
    uint64_t signature_id;
    uint32_t effects;
    uint32_t reserved_effects;
    uint64_t required_capabilities;
    OgmaPrimitiveF64Thunk primitive_f64_thunk;
    OgmaNumberHandleF64Thunk number_handle_f64_thunk;
    OgmaHostCallback generic_callback;
    void *user_data;
} OgmaHostFunctionDescriptor;

typedef struct {
    uint64_t registry_id;
    uint32_t slot_index;
    uint32_t slot_generation;
    uint16_t abi_major;
    uint16_t abi_minor;
} OgmaHostFunctionSlot;

#ifdef __cplusplus
}
#endif

#endif
