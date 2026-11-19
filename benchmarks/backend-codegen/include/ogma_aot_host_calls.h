#ifndef OGMA_AOT_HOST_CALLS_H
#define OGMA_AOT_HOST_CALLS_H

#include <stddef.h>
#include <stdint.h>
#include <ogma_host_dispatch.h>

#ifdef __cplusplus
extern "C" {
#endif

#define OGMA_AOT_HOST_ABI_MAJOR UINT16_C(1)
#define OGMA_AOT_HOST_ABI_MINOR UINT16_C(0)
#define OGMA_AOT_HOST_DESCRIPTOR_VERSION UINT16_C(1)
#define OGMA_AOT_SIGNATURE_ID_F64_BINARY UINT64_C(0x1001)
#define OGMA_AOT_SIGNATURE_ID_NUMBER_HANDLE_BINARY UINT64_C(0x1002)
#define OGMA_AOT_MAX_ENTRIES UINT32_C(4096)
#define OGMA_AOT_MAX_ARGUMENTS ((size_t)4096)

#define OGMA_AOT_VALUE_UNDEFINED UINT32_C(0)
#define OGMA_AOT_VALUE_NUMBER UINT32_C(1)
#define OGMA_AOT_VALUE_BOOLEAN UINT32_C(2)
#define OGMA_AOT_VALUE_HANDLE UINT32_C(3)

#define OGMA_AOT_COMPLETION_NORMAL UINT32_C(0)
#define OGMA_AOT_COMPLETION_THROW UINT32_C(1)

#define OGMA_AOT_ERROR_NONE UINT32_C(0)
#define OGMA_AOT_ERROR_TYPE UINT32_C(1)
#define OGMA_AOT_ERROR_RANGE UINT32_C(2)
#define OGMA_AOT_ERROR_CAPABILITY UINT32_C(3)
#define OGMA_AOT_ERROR_THROWN UINT32_C(4)
#define OGMA_AOT_ERROR_CANCELLED UINT32_C(5)
#define OGMA_AOT_ERROR_ASYNC_UNSUPPORTED UINT32_C(6)
#define OGMA_AOT_ERROR_REFERENCE UINT32_C(7)

#define OGMA_AOT_CAPABILITY_MATH (UINT64_C(1) << 0)

#define OGMA_AOT_COMPLEX_EFFECT_MASK ( \
    OGMA_HOST_EFFECT_MAY_ALLOCATE | \
    OGMA_HOST_EFFECT_MAY_GC | \
    OGMA_HOST_EFFECT_MAY_THROW | \
    OGMA_HOST_EFFECT_MAY_SUSPEND | \
    OGMA_HOST_EFFECT_MAY_REENTER)

typedef union {
    double number;
    uint8_t boolean;
    OgmaHandle handle;
} OgmaAotValuePayloadV1;

typedef struct {
    uint32_t tag;
    uint32_t reserved;
    OgmaAotValuePayloadV1 payload;
} OgmaAotValueV1;

typedef struct {
    uint32_t kind;
    uint32_t error_kind;
    uint64_t details;
    OgmaAotValueV1 value;
} OgmaAotCompletionV1;

/* The AOT entry points and table are C ABI. The host owns user_data and the
 * backing allocations for entries; generated code may only pass these values
 * back to the supplied thunks and must not inspect user_data. */
typedef OgmaStatus (*OgmaAotPrimitiveF64Thunk)(
    void *user_data, uint64_t capabilities, double left, double right,
    OgmaAotCompletionV1 *out_completion);

typedef OgmaStatus (*OgmaAotNumberHandleF64Thunk)(
    void *user_data, uint64_t capabilities, OgmaHandle left, OgmaHandle right,
    OgmaAotCompletionV1 *out_completion);

typedef OgmaStatus (*OgmaAotGenericThunk)(
    void *user_data, uint64_t capabilities, const OgmaAotValueV1 *args,
    size_t args_len, OgmaAotCompletionV1 *out_completion);

typedef struct {
    uint32_t struct_size;
    uint16_t descriptor_version;
    uint16_t abi_major;
    uint16_t abi_minor;
    uint16_t reserved;
    uint64_t function_id;
    uint32_t signature;
    uint32_t effects;
    uint64_t signature_id;
    uint64_t required_capabilities;
    OgmaAotPrimitiveF64Thunk primitive_f64_thunk;
    OgmaAotNumberHandleF64Thunk number_handle_f64_thunk;
    OgmaAotGenericThunk generic_thunk;
    void *user_data;
} OgmaAotHostFunctionEntryV1;

typedef struct {
    uint32_t struct_size;
    uint16_t abi_major;
    uint16_t abi_minor;
    uint32_t entry_size;
    uint32_t entry_count;
    const OgmaAotHostFunctionEntryV1 *entries;
    uint64_t capabilities;
    uint64_t table_version;
    uint64_t reserved;
} OgmaAotHostFunctionTableV1;

/* These helpers are linked only into the M0 proof executable. Generated
 * object files call them using the C ABI; they validate the table and dispatch
 * to the selected typed thunk or generic fallback. */
int64_t ogma_aot_dispatch_f64(
    int64_t table_address, int32_t slot, double left, double right,
    uint64_t capabilities);
int64_t ogma_aot_dispatch_handles(
    int64_t table_address, int32_t slot, int64_t left_handle_address,
    int64_t right_handle_address, uint64_t capabilities);

#ifdef __cplusplus
}
#endif

#endif
