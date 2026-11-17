// Copyright (C) 2026 Ogma test fixture.
// Additional source provenance may precede Test262 metadata.
/*---
description: Metadata can follow license and generated-source comments.
features: [Array]
info: |
    Example algorithm:
      1. This is an indented block scalar.
      2. It must not consume later root-level metadata.
flags: [noStrict]
---*/
assert.sameValue([1, 2].length, 2);
