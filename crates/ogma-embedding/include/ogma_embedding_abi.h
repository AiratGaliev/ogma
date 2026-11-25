#ifndef OGMA_EMBEDDING_ABI_H
#define OGMA_EMBEDDING_ABI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define OGMA_ABI_MAJOR 1u
#define OGMA_ABI_MINOR 0u
#define OGMA_ABI_PATCH 0u
#define OGMA_HOST_DESCRIPTOR_VERSION 1u

#define OGMA_FEATURE_OPAQUE_HANDLES      (UINT64_C(1) << 0)
#define OGMA_FEATURE_ROOT_SCOPES         (UINT64_C(1) << 1)
#define OGMA_FEATURE_PERSISTENT_HANDLES  (UINT64_C(1) << 2)
#define OGMA_FEATURE_CALLBACKS           (UINT64_C(1) << 3)
#define OGMA_FEATURE_COMPLETIONS         (UINT64_C(1) << 4)

#define OGMA_STATUS_OK                   0u
#define OGMA_STATUS_INVALID_ARGUMENT     1u
#define OGMA_STATUS_INCOMPATIBLE_ABI     2u
#define OGMA_STATUS_UNSUPPORTED_FEATURE  3u
#define OGMA_STATUS_WRONG_THREAD         4u
#define OGMA_STATUS_STALE_HANDLE         5u
#define OGMA_STATUS_TYPE_MISMATCH        6u
#define OGMA_STATUS_OUT_OF_RANGE         7u
#define OGMA_STATUS_REENTRANCY_LIMIT     8u
#define OGMA_STATUS_CALLBACK_ERROR       9u
#define OGMA_STATUS_INVALID_COMPLETION   10u
#define OGMA_STATUS_BUSY                 11u
#define OGMA_STATUS_PANIC                12u
#define OGMA_STATUS_INTERNAL_ERROR       13u

#define OGMA_VALUE_UNDEFINED 0u
#define OGMA_VALUE_NULL      1u
#define OGMA_VALUE_BOOLEAN   2u
#define OGMA_VALUE_NUMBER    3u

#define OGMA_COMPLETION_NORMAL 0u
#define OGMA_COMPLETION_THROW  1u

#define OGMA_HOST_SIGNATURE_GENERIC    0u
#define OGMA_HOST_SIGNATURE_F64_BINARY 0x1001u

#define OGMA_HOST_EFFECT_MAY_ALLOCATE (1u << 0)
#define OGMA_HOST_EFFECT_MAY_GC       (1u << 1)
#define OGMA_HOST_EFFECT_MAY_THROW    (1u << 2)
#define OGMA_HOST_EFFECT_MAY_SUSPEND  (1u << 3)
#define OGMA_HOST_EFFECT_MAY_REENTER  (1u << 4)

typedef uint32_t OgmaStatus;

typedef struct OgmaAbiVersion {
    uint16_t major;
    uint16_t minor;
    uint16_t patch;
    uint16_t reserved;
    uint32_t struct_size;
    uint32_t reserved2;
    uint64_t features;
} OgmaAbiVersion;

typedef struct OgmaHostValue {
    uint32_t kind;
    uint32_t reserved;
    uint64_t payload;
} OgmaHostValue;

typedef struct OgmaCompletion {
    uint32_t kind;
    uint32_t reserved;
    OgmaHostValue value;
} OgmaCompletion;

typedef OgmaStatus (*OgmaGenericHostCallback)(
    const OgmaHostValue *arguments,
    size_t arguments_len,
    OgmaCompletion *out_completion,
    void *user_data);

typedef OgmaStatus (*OgmaPrimitiveF64Thunk)(
    double left,
    double right,
    double *out_result,
    void *user_data);

typedef struct OgmaHostFunctionDescriptor {
    uint32_t struct_size;
    uint16_t descriptor_version;
    uint16_t abi_major;
    uint16_t abi_minor;
    uint16_t reserved;
    uint64_t function_id;
    const uint8_t *name;
    size_t name_len;
    uint32_t signature;
    uint32_t effects;
    uint64_t required_capabilities;
    OgmaGenericHostCallback generic_callback;
    OgmaPrimitiveF64Thunk primitive_f64_thunk;
    void *user_data;
} OgmaHostFunctionDescriptor;

OgmaAbiVersion ogma_abi_version(void);
OgmaStatus ogma_abi_negotiate(
    uint16_t required_major,
    uint16_t minimum_minor,
    uint64_t required_features,
    OgmaAbiVersion *out_version);
OgmaStatus ogma_primitive_add_f64(double left, double right, double *out_result);

#if defined(__cplusplus)
static_assert(sizeof(OgmaAbiVersion) == 24, "OgmaAbiVersion ABI layout mismatch");
static_assert(sizeof(OgmaHostValue) == 16, "OgmaHostValue ABI layout mismatch");
static_assert(sizeof(OgmaCompletion) == 24, "OgmaCompletion ABI layout mismatch");
#else
_Static_assert(sizeof(OgmaAbiVersion) == 24, "OgmaAbiVersion ABI layout mismatch");
_Static_assert(sizeof(OgmaHostValue) == 16, "OgmaHostValue ABI layout mismatch");
_Static_assert(sizeof(OgmaCompletion) == 24, "OgmaCompletion ABI layout mismatch");
#endif

#ifdef __cplusplus
}
#endif

#endif
