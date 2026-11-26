# ESM module graph contract

## Scope

M1 implements a host-resolved, static ECMAScript module graph. The embedding
host supplies canonical module identities, source/bytecode artifacts and
specifier resolution through `ModuleResolver`. The core engine does not read
the filesystem, access the network, inspect `package.json`, or apply Node.js
package-resolution rules.

The module graph is scoped to one `Context`. A Context instantiates one graph;
repeated evaluation of an entry already in that graph reuses its module
instances and does not execute an already evaluated module again. Loading a
different entry graph into that Context is rejected rather than silently
creating a second cache. Create a new Context when a distinct graph is needed.

## Resolution and identity

- The entry module ID and every resolved dependency ID are canonical IDs
  supplied by the resolver. A specifier is resolved relative to the importing
  module ID.
- The resolver is responsible for deterministic identity and source retrieval.
  Two requests that resolve to the same canonical ID refer to one module
  instance in the graph.
- Resolver failures, duplicate identities with inconsistent descriptors,
  missing dependencies and malformed descriptors fail graph construction
  explicitly. No fallback to ambient host resolution is performed.
- Static requests are discovered before linking. Dynamic `import()` is not
  part of this profile.

## Instantiation and linking

The engine loads and verifies the executable artifacts, then creates every
module environment and entry closure before linking any import or evaluating
any module body. This ordering permits cyclic graphs to be instantiated
without evaluating a partially linked module.

Named and default imports are read-only aliases to the target module's lexical
binding. They remain live: reads observe the current value of the exporting
binding instead of a copied value. Writes through an import are rejected by
the binding contract.

Local named exports, named function declarations exported as `default`,
local export aliases (including `export { value as default }`), named
re-exports and export-star resolution are supported. Explicit exports take
precedence over star exports, the default export is not propagated by
`export *`, and ambiguous star exports fail deterministically. Default export
expressions and anonymous default declarations are outside the current
frontend subset. Namespace imports/exports are rejected until namespace
exotic objects are implemented.

## Evaluation

Modules are evaluated in deterministic dependency order. The evaluator tracks
linked, evaluating, evaluated and failed states. Encountering an evaluating
module during a cycle is a back-edge, not a second evaluation. An evaluated
module is skipped on later entry evaluation.

A thrown module completion marks the module failed and is returned as a
persistent completion handle owned by the caller. The caller must release that
handle through the Context API. Interrupted evaluation is reported explicitly;
it is not reported as successful evaluation.

## M1 exclusions

This contract does not claim support for CommonJS, Node.js package resolution,
namespace exotic objects, dynamic import, top-level await, import attributes,
or the full set of ECMAScript early-error and host integration semantics.
These features require separate compatibility work and must fail closed where
the current frontend/compiler profile cannot represent them.
