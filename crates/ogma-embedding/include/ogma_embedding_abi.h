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

#define OGMA_COMPLETION_NORMAL   0u
#define OGMA_COMPLETION_THROW    1u
#define OGMA_COMPLETION_RETURN   2u
#define OGMA_COMPLETION_BREAK    3u
#define OGMA_COMPLETION_CONTINUE 4u

#define OGMA_HOST_SIGNATURE_GENERIC              0u
#define OGMA_HOST_SIGNATURE_F64_BINARY           1u
#define OGMA_HOST_SIGNATURE_NUMBER_HANDLE_BINARY 2u

#define OGMA_HOST_SIGNATURE_ID_GENERIC              UINT64_C(0)
#define OGMA_HOST_SIGNATURE_ID_F64_BINARY           UINT64_C(0x1001)
#define OGMA_HOST_SIGNATURE_ID_NUMBER_HANDLE_BINARY UINT64_C(0x1002)

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

/* Opaque owner-thread Runtime pointer; its Rust implementation is private. */
typedef struct OgmaRuntime OgmaRuntime;

/* Opaque, generation-checked ABI records. Each is exactly 40 bytes. */
typedef struct OgmaContextHandle { uint64_t opaque[5]; } OgmaContextHandle;
typedef struct OgmaHandle { uint64_t opaque[5]; } OgmaHandle;
typedef struct OgmaRootScopeHandle { uint64_t opaque[5]; } OgmaRootScopeHandle;
typedef struct OgmaPersistentHandle { uint64_t opaque[5]; } OgmaPersistentHandle;
typedef struct OgmaCallbackHandle { uint64_t opaque[5]; } OgmaCallbackHandle;

/* Script completion kind is separate from OgmaStatus transport status. */
typedef struct OgmaCompletion {
    uint32_t kind;
    uint32_t reserved;
    OgmaHandle value;
} OgmaCompletion;

/* Callback argument handles are borrowed and valid only during the callback. */
typedef OgmaStatus (*OgmaHostCallback)(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    const OgmaHandle *arguments,
    size_t arguments_len,
    OgmaCompletion *completion,
    void *user_data);

typedef OgmaStatus (*OgmaPrimitiveF64Thunk)(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    double left,
    double right,
    double *out_result,
    void *user_data);

typedef OgmaStatus (*OgmaNumberHandleF64Thunk)(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    OgmaHandle left,
    OgmaHandle right,
    double *out_result,
    void *user_data);

/* Canonical ABI-v1 typed host-function descriptor. */
typedef struct OgmaHostFunctionDescriptor {
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

typedef struct OgmaHostFunctionSlot {
    uint64_t registry_id;
    uint32_t slot_index;
    uint32_t slot_generation;
    uint16_t abi_major;
    uint16_t abi_minor;
} OgmaHostFunctionSlot;

/* Active-callback-only helpers. All handles expire when their callback ends. */
OgmaStatus ogma_context_collect_unrooted(
    const OgmaRuntime *runtime,
    OgmaContextHandle context);
OgmaStatus ogma_context_create_number(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    double number,
    OgmaHandle *out_value);
OgmaStatus ogma_context_get_number(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    OgmaHandle value,
    double *out_number);
OgmaStatus ogma_context_create_boolean(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    uint8_t boolean,
    OgmaHandle *out_value);
OgmaStatus ogma_context_get_boolean(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    OgmaHandle value,
    uint8_t *out_boolean);
OgmaStatus ogma_context_get_i32(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    OgmaHandle value,
    int32_t *out_integer);
OgmaStatus ogma_context_sum_numbers(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    const OgmaHandle *handles,
    size_t count,
    double *out_sum);

OgmaAbiVersion ogma_abi_version(void);
OgmaStatus ogma_abi_negotiate(
    uint16_t required_major,
    uint16_t minimum_minor,
    uint64_t required_features,
    OgmaAbiVersion *out_version);
OgmaStatus ogma_primitive_add_f64(double left, double right, double *out_result);

#if defined(__cplusplus)
static_assert(sizeof(OgmaAbiVersion) == 24, "OgmaAbiVersion ABI layout mismatch");
static_assert(sizeof(OgmaContextHandle) == 40, "OgmaContextHandle ABI layout mismatch");
static_assert(sizeof(OgmaHandle) == 40, "OgmaHandle ABI layout mismatch");
static_assert(sizeof(OgmaRootScopeHandle) == 40, "OgmaRootScopeHandle ABI layout mismatch");
static_assert(sizeof(OgmaPersistentHandle) == 40, "OgmaPersistentHandle ABI layout mismatch");
static_assert(sizeof(OgmaCallbackHandle) == 40, "OgmaCallbackHandle ABI layout mismatch");
static_assert(sizeof(OgmaCompletion) == 48, "OgmaCompletion ABI layout mismatch");
static_assert(sizeof(OgmaHostFunctionDescriptor) == 216, "OgmaHostFunctionDescriptor ABI layout mismatch");
static_assert(sizeof(OgmaHostFunctionSlot) == 24, "OgmaHostFunctionSlot ABI layout mismatch");
#else
_Static_assert(sizeof(OgmaAbiVersion) == 24, "OgmaAbiVersion ABI layout mismatch");
_Static_assert(sizeof(OgmaContextHandle) == 40, "OgmaContextHandle ABI layout mismatch");
_Static_assert(sizeof(OgmaHandle) == 40, "OgmaHandle ABI layout mismatch");
_Static_assert(sizeof(OgmaRootScopeHandle) == 40, "OgmaRootScopeHandle ABI layout mismatch");
_Static_assert(sizeof(OgmaPersistentHandle) == 40, "OgmaPersistentHandle ABI layout mismatch");
_Static_assert(sizeof(OgmaCallbackHandle) == 40, "OgmaCallbackHandle ABI layout mismatch");
_Static_assert(sizeof(OgmaCompletion) == 48, "OgmaCompletion ABI layout mismatch");
_Static_assert(sizeof(OgmaHostFunctionDescriptor) == 216, "OgmaHostFunctionDescriptor ABI layout mismatch");
_Static_assert(sizeof(OgmaHostFunctionSlot) == 24, "OgmaHostFunctionSlot ABI layout mismatch");
#endif

#ifdef __cplusplus
}
#endif

#endif
