#include <stddef.h>
#include "ogma_aot_host_calls.h"

_Static_assert(sizeof(OgmaAotValuePayloadV1) == 40, "AOT payload ABI-v1 layout changed");
_Static_assert(sizeof(OgmaAotValueV1) == 48, "AOT value ABI-v1 layout changed");
_Static_assert(offsetof(OgmaAotValueV1, payload) == 8, "AOT value payload offset changed");
_Static_assert(sizeof(OgmaAotCompletionV1) == 64, "AOT completion ABI-v1 layout changed");
_Static_assert(offsetof(OgmaAotCompletionV1, value) == 16, "AOT completion value offset changed");
_Static_assert(sizeof(OgmaAotHostFunctionEntryV1) == 80, "AOT entry ABI-v1 layout changed");
_Static_assert(offsetof(OgmaAotHostFunctionEntryV1, function_id) == 16, "AOT function ID offset changed");
_Static_assert(offsetof(OgmaAotHostFunctionEntryV1, signature_id) == 32, "AOT signature ID offset changed");
_Static_assert(offsetof(OgmaAotHostFunctionEntryV1, primitive_f64_thunk) == 48, "AOT typed thunk offset changed");
_Static_assert(offsetof(OgmaAotHostFunctionEntryV1, user_data) == 72, "AOT user data offset changed");
_Static_assert(sizeof(OgmaAotHostFunctionTableV1) == 48, "AOT table ABI-v1 layout changed");
_Static_assert(offsetof(OgmaAotHostFunctionTableV1, entries) == 16, "AOT entries offset changed");
_Static_assert(offsetof(OgmaAotHostFunctionTableV1, table_version) == 32, "AOT table version offset changed");

int main(void) {
    return 0;
}
