#include "ogma_abi.h"

#include <assert.h>
#include <stddef.h>
#include <stdint.h>

_Static_assert(sizeof(OgmaContextHandle) == 5 * sizeof(uint64_t), "context token layout changed");
_Static_assert(sizeof(OgmaHandle) == 5 * sizeof(uint64_t), "value token layout changed");
_Static_assert(sizeof(OgmaRootScopeHandle) == 5 * sizeof(uint64_t), "root token layout changed");
_Static_assert(sizeof(OgmaPersistentHandle) == 5 * sizeof(uint64_t), "persistent token layout changed");
_Static_assert(sizeof(OgmaCallbackHandle) == 5 * sizeof(uint64_t), "callback token layout changed");
_Static_assert(sizeof(OgmaAbiVersion) == 24, "ABI version descriptor layout changed");
_Static_assert(offsetof(OgmaAbiVersion, features) == 16, "ABI feature offset changed");
_Static_assert(sizeof(OgmaCompletion) == 48, "completion layout changed");
_Static_assert(offsetof(OgmaCompletion, value) == 8, "completion value offset changed");

static OgmaStatus echo_callback(
    const OgmaRuntime *runtime,
    OgmaContextHandle context,
    const OgmaHandle *args,
    size_t args_len,
    OgmaCompletion *completion,
    void *user_data
) {
    (void)runtime;
    (void)context;
    (void)user_data;

    if (args == NULL || args_len != 1 || completion == NULL) {
        return OGMA_STATUS_INVALID_ARGUMENT;
    }

    completion->kind = OGMA_COMPLETION_NORMAL;
    completion->reserved = 0;
    completion->value = args[0];
    return OGMA_STATUS_OK;
}

int main(void) {
    const OgmaAbiVersion version = ogma_abi_version();
    assert(version.major == 1);
    assert(version.minor == 0);
    assert(version.patch == 0);
    assert(version.struct_size == sizeof(OgmaAbiVersion));
    assert((version.features & OGMA_FEATURE_OPAQUE_HANDLES) != 0);
    assert((version.features & OGMA_FEATURE_MOCK_PROOF) != 0);

    OgmaAbiVersion negotiated = {0};
    const uint64_t required_features =
        OGMA_FEATURE_OPAQUE_HANDLES |
        OGMA_FEATURE_ROOT_SCOPES |
        OGMA_FEATURE_PERSISTENT_HANDLES |
        OGMA_FEATURE_CALLBACKS |
        OGMA_FEATURE_COMPLETIONS |
        OGMA_FEATURE_MOCK_PROOF;
    assert(ogma_abi_negotiate(1, 0, required_features, &negotiated) == OGMA_STATUS_OK);
    assert(negotiated.major == version.major);

    OgmaRuntime *runtime = NULL;
    assert(ogma_runtime_create(&runtime) == OGMA_STATUS_OK);
    assert(runtime != NULL);

    OgmaContextHandle context = {0};
    assert(ogma_context_create(runtime, &context) == OGMA_STATUS_OK);

    OgmaRootScopeHandle scope = {0};
    assert(ogma_root_scope_open(runtime, context, &scope) == OGMA_STATUS_OK);

    OgmaHandle value = {0};
    assert(ogma_context_create_number(runtime, context, 21.0, &value) == OGMA_STATUS_OK);
    assert(ogma_root_scope_add(runtime, context, scope, value) == OGMA_STATUS_OK);

    OgmaPersistentHandle persistent = {0};
    assert(ogma_persistent_create(runtime, context, value, &persistent) == OGMA_STATUS_OK);

    OgmaCallbackHandle callback = {0};
    assert(ogma_callback_register(runtime, context, echo_callback, NULL, &callback) == OGMA_STATUS_OK);

    OgmaHandle function = {0};
    assert(ogma_mock_function_create(runtime, context, callback, &function) == OGMA_STATUS_OK);
    assert(ogma_root_scope_add(runtime, context, scope, function) == OGMA_STATUS_OK);

    OgmaCompletion completion = {0};
    assert(ogma_context_call(runtime, context, function, &value, 1, &completion) == OGMA_STATUS_OK);
    assert(completion.kind == OGMA_COMPLETION_NORMAL);

    double number = 0.0;
    assert(ogma_context_get_number(runtime, context, completion.value, &number) == OGMA_STATUS_OK);
    assert(number == 21.0);

    OgmaHandle retained = {0};
    assert(ogma_persistent_get(runtime, context, persistent, &retained) == OGMA_STATUS_OK);
    assert(ogma_context_get_number(runtime, context, retained, &number) == OGMA_STATUS_OK);
    assert(number == 21.0);

    assert(ogma_callback_unregister(runtime, context, callback) == OGMA_STATUS_OK);
    completion = (OgmaCompletion){0};
    assert(ogma_context_call(runtime, context, function, &value, 1, &completion) == OGMA_STATUS_STALE_HANDLE);
    assert(completion.kind == OGMA_COMPLETION_NORMAL);
    assert(completion.value.opaque[0] == 0);

    assert(ogma_root_scope_close(runtime, context, scope) == OGMA_STATUS_OK);
    assert(ogma_persistent_release(runtime, context, persistent) == OGMA_STATUS_OK);
    assert(ogma_context_collect_unrooted(runtime, context) == OGMA_STATUS_OK);
    assert(ogma_context_get_number(runtime, context, retained, &number) == OGMA_STATUS_STALE_HANDLE);

    assert(ogma_context_destroy(runtime, context) == OGMA_STATUS_OK);
    assert(ogma_runtime_destroy(&runtime) == OGMA_STATUS_OK);
    assert(runtime == NULL);
    return 0;
}
