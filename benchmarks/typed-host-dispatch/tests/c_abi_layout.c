#include <stddef.h>
#include <ogma_host_dispatch.h>

_Static_assert(sizeof(OgmaHostFunctionDescriptor) == 216,
               "OgmaHostFunctionDescriptor layout drifted");
_Static_assert(offsetof(OgmaHostFunctionDescriptor, function_id) == 16,
               "function_id offset drifted");
_Static_assert(offsetof(OgmaHostFunctionDescriptor, signature_id) == 160,
               "signature_id offset drifted");
_Static_assert(offsetof(OgmaHostFunctionDescriptor, required_capabilities) == 176,
               "required_capabilities offset drifted");
_Static_assert(offsetof(OgmaHostFunctionDescriptor, user_data) == 208,
               "user_data offset drifted");
_Static_assert(sizeof(OgmaHostFunctionSlot) == 24,
               "OgmaHostFunctionSlot layout drifted");
_Static_assert(offsetof(OgmaHostFunctionSlot, abi_major) == 16,
               "slot ABI version offset drifted");

int main(void) {
    return 0;
}
