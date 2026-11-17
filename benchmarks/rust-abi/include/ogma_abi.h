#ifndef OGMA_ABI_H
#define OGMA_ABI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque owner pointer; never cast to or interpret as a JS object pointer. */
typedef struct OgmaRuntime OgmaRuntime;
typedef struct { uint64_t opaque[5]; } OgmaContextHandle;
typedef struct { uint64_t opaque[5]; } OgmaHandle;
typedef struct { uint64_t opaque[5]; } OgmaRootScopeHandle;
typedef struct { uint64_t opaque[5]; } OgmaPersistentHandle;
typedef struct { uint64_t opaque[5]; } OgmaCallbackHandle;

typedef uint32_t OgmaStatus;
#define OGMA_STATUS_OK                  ((OgmaStatus)0)
#define OGMA_STATUS_INVALID_ARGUMENT    ((OgmaStatus)1)
#define OGMA_STATUS_INCOMPATIBLE_ABI     ((OgmaStatus)2)
#define OGMA_STATUS_UNSUPPORTED_FEATURE ((OgmaStatus)3)
#define OGMA_STATUS_WRONG_THREAD         ((OgmaStatus)4)
#define OGMA_STATUS_STALE_HANDLE         ((OgmaStatus)5)
#define OGMA_STATUS_TYPE_MISMATCH        ((OgmaStatus)6)
#define OGMA_STATUS_OUT_OF_RANGE         ((OgmaStatus)7)
#define OGMA_STATUS_REENTRANCY_LIMIT     ((OgmaStatus)8)
#define OGMA_STATUS_CALLBACK_ERROR       ((OgmaStatus)9)
#define OGMA_STATUS_INVALID_COMPLETION   ((OgmaStatus)10)
#define OGMA_STATUS_BUSY                 ((OgmaStatus)11)
#define OGMA_STATUS_PANIC                ((OgmaStatus)12)
#define OGMA_STATUS_INTERNAL_ERROR       ((OgmaStatus)13)

#define OGMA_FEATURE_OPAQUE_HANDLES      (UINT64_C(1) << 0)
#define OGMA_FEATURE_ROOT_SCOPES         (UINT64_C(1) << 1)
#define OGMA_FEATURE_PERSISTENT_HANDLES  (UINT64_C(1) << 2)
#define OGMA_FEATURE_CALLBACKS           (UINT64_C(1) << 3)
#define OGMA_FEATURE_COMPLETIONS         (UINT64_C(1) << 4)
#define OGMA_FEATURE_MOCK_PROOF          (UINT64_C(1) << 63)

typedef struct {
    uint16_t major;
    uint16_t minor;
    uint16_t patch;
    uint16_t reserved;
    uint32_t struct_size;
    uint32_t reserved2;
    uint64_t features;
} OgmaAbiVersion;

#define OGMA_COMPLETION_NORMAL   UINT32_C(0)
#define OGMA_COMPLETION_THROW    UINT32_C(1)
#define OGMA_COMPLETION_RETURN   UINT32_C(2)
#define OGMA_COMPLETION_BREAK    UINT32_C(3)
#define OGMA_COMPLETION_CONTINUE UINT32_C(4)
typedef struct {
    uint32_t kind;
    uint32_t reserved;
    OgmaHandle value;
} OgmaCompletion;

/* args/handles are borrowed only during the callback. Persist retained values.
   user_data remains host-owned and valid until callback unregistration. */
typedef OgmaStatus (*OgmaHostCallback)(
    const OgmaRuntime *runtime, OgmaContextHandle context,
    const OgmaHandle *args, size_t args_len,
    OgmaCompletion *completion, void *user_data);

OgmaAbiVersion ogma_abi_version(void);
OgmaStatus ogma_abi_negotiate(uint16_t required_major, uint16_t minimum_minor,
                              uint64_t required_features, OgmaAbiVersion *out_version);
OgmaStatus ogma_runtime_create(OgmaRuntime **out_runtime);
OgmaStatus ogma_runtime_destroy(OgmaRuntime **runtime_slot);
OgmaStatus ogma_context_create(const OgmaRuntime *runtime, OgmaContextHandle *out_context);
OgmaStatus ogma_context_destroy(const OgmaRuntime *runtime, OgmaContextHandle context);

OgmaStatus ogma_context_create_number(const OgmaRuntime *runtime, OgmaContextHandle context,
                                      double value, OgmaHandle *out_value);
OgmaStatus ogma_context_create_boolean(const OgmaRuntime *runtime, OgmaContextHandle context,
                                       uint8_t value, OgmaHandle *out_value);
OgmaStatus ogma_context_get_number(const OgmaRuntime *runtime, OgmaContextHandle context,
                                   OgmaHandle handle, double *out_value);
OgmaStatus ogma_context_get_boolean(const OgmaRuntime *runtime, OgmaContextHandle context,
                                    OgmaHandle handle, uint8_t *out_value);
OgmaStatus ogma_context_get_i32(const OgmaRuntime *runtime, OgmaContextHandle context,
                                OgmaHandle handle, int32_t *out_value);
OgmaStatus ogma_primitive_add_f64(double left, double right, double *out_value);
OgmaStatus ogma_context_sum_numbers(const OgmaRuntime *runtime, OgmaContextHandle context,
                                    const OgmaHandle *handles, size_t count, double *out_sum);

OgmaStatus ogma_root_scope_open(const OgmaRuntime *runtime, OgmaContextHandle context,
                                OgmaRootScopeHandle *out_scope);
OgmaStatus ogma_root_scope_add(const OgmaRuntime *runtime, OgmaContextHandle context,
                               OgmaRootScopeHandle scope, OgmaHandle value);
OgmaStatus ogma_root_scope_close(const OgmaRuntime *runtime, OgmaContextHandle context,
                                 OgmaRootScopeHandle scope);
OgmaStatus ogma_persistent_create(const OgmaRuntime *runtime, OgmaContextHandle context,
                                  OgmaHandle value, OgmaPersistentHandle *out_persistent);
OgmaStatus ogma_persistent_get(const OgmaRuntime *runtime, OgmaContextHandle context,
                               OgmaPersistentHandle persistent, OgmaHandle *out_value);
OgmaStatus ogma_persistent_release(const OgmaRuntime *runtime, OgmaContextHandle context,
                                   OgmaPersistentHandle persistent);
OgmaStatus ogma_context_collect_unrooted(const OgmaRuntime *runtime, OgmaContextHandle context);

OgmaStatus ogma_callback_register(const OgmaRuntime *runtime, OgmaContextHandle context,
                                  OgmaHostCallback callback, void *user_data,
                                  OgmaCallbackHandle *out_callback);
OgmaStatus ogma_callback_unregister(const OgmaRuntime *runtime, OgmaContextHandle context,
                                    OgmaCallbackHandle callback);
OgmaStatus ogma_mock_function_create(const OgmaRuntime *runtime, OgmaContextHandle context,
                                     OgmaCallbackHandle callback, OgmaHandle *out_function);
OgmaStatus ogma_context_call(const OgmaRuntime *runtime, OgmaContextHandle context,
                             OgmaHandle function, const OgmaHandle *args, size_t args_len,
                             OgmaCompletion *completion);

#ifdef __cplusplus
}
#endif
#endif
