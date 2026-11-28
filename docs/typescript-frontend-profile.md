# Ogma TypeScript Frontend Profile v1

**Status:** Implemented by OGM-54.
**Profile ID:** ogma.typescript.v1

## Purpose and compatibility boundary

Ogma accepts a deliberately bounded TypeScript syntax profile. TypeScript is a compile-time frontend only: Ogma does not execute TypeScript syntax, add runtime type objects, perform static type checking, or inject browser, DOM, or Node.js globals. Type annotations and other supported type-only syntax are erased before the existing JavaScript semantic snapshot, Core IR verifier, bytecode compiler and interpreter pipeline.

The language compatibility reference is Microsoft TypeScript 7.0.2, pinned to upstream revision 1e4744d68260a7cb91b62b12edc3f6a2187faaf1. This pins the grammar/test oracle, not a dependency on the TypeScript compiler or a claim of full TypeScript 7 conformance.

## Pinned implementation

| Component | Pin | Role |
| --- | --- | --- |
| Oxc parser | 0.158.0 | TypeScript and TSX parsing |
| Oxc transformer | 0.158.0 | In-place TypeScript type erasure |
| Oxc source revision | 74e5e665ab8227c3d43abf3ce0e4383830e80c12 | Immutable upstream source revision |
| Rust edition/MSRV | 2024 / 1.100 | Ogma workspace contract |
| Oxc license/MSRV | MIT / 1.97 | Compatible with Ogma MSRV 1.100 |

The Rust dependencies are exact-version pinned in the frontend manifest and Cargo.lock. Oxc parser and transformer use the same version and the same Oxc AST/semantic crates. The implementation consumes the caller-owned Oxc arena and mutates the parsed Program in place; it does not build a parallel TypeScript AST or serialize/reparse transformed source.

## Supported syntax

The initial profile accepts type-only syntax that Oxc 0.158.0 can erase without emitting new runtime constructs, subject to the existing Ogma JavaScript/Core IR subset:

- Type annotations on variables, parameters, return types, and class members.
- Type aliases and interfaces, including generic declarations and type parameters.
- Generic call/type arguments and type parameter constraints/defaults.
- Type assertions (as and angle-bracket forms where permitted by TypeScript grammar), non-null assertions, and satisfies expressions.
- Type-only import/export declarations and specifiers.
- Ambient declare declarations that have no runtime implementation.
- TSX source files which contain no JSX expression after parsing.

No full semantic type checker is included. The frontend validates syntax and performs the pinned transform; it does not prove assignability, generic constraints, overload resolution, declaration merging correctness, or library type compatibility.

## Explicitly unsupported syntax

The frontend rejects runtime constructs for which the current Core IR/runtime contract does not define equivalent semantics:

- Runtime enums, including const enums.
- Runtime namespaces and module declarations.
- Decorators and decorator metadata.
- Constructor parameter properties.
- TypeScript import-equals and export-assignment declarations.
- JSX elements and fragments. TSX parsing is supported, but the consumer build owns JSX lowering. For the pinned Solid Native consumer, run its declared vite-plugin-solid pipeline before passing the emitted JavaScript to Ogma. Ogma does not substitute React JSX lowering.
- Declaration files (.d.ts, .d.mts and .d.cts) as executable entry points.
- Syntax outside the implemented Ogma Core IR/runtime subset, even when it is valid TypeScript.

Unsupported constructs fail closed with stable diagnostic codes and original-source labels. Type erasure transformer warnings are treated as errors so partially supported syntax cannot silently pass to Core IR.

## Source identity and provenance

SourceFile remains the authoritative owner of immutable source text, stable SourceId, URI, content digest, UTF-8 byte offsets, and editor-facing UTF-16 line/column positions. Oxc's TypeScript transform mutates the same arena-owned AST; type-only nodes are removed and source-authored runtime nodes retain their original Oxc byte spans. The final owned syntax projection and semantic snapshot are rebuilt from the transformed Program and the same SourceFile.

The v1 profile intentionally rejects syntax that would need generated runtime nodes (enums, namespaces, parameter properties, decorators, JSX lowering). Therefore no generated runtime instruction is emitted by this transform, and there is no synthetic-source offset mapping to invent. A future profile that adds runtime-generating transforms must attach an explicit source-origin span to every generated Core IR node and test it. Ogma's existing Core IR debug/source spans remain the source mapping contract; Oxc code generation/source-map output is not run for this path.

## Module and package boundaries

The existing compiler and static module resolver remain authoritative. Type-only imports are erased before module graph construction and therefore do not trigger filesystem resolution. Runtime module requests continue to follow the existing CLI/toolchain contract; package-manager and Node-compatible resolution remain owned by OGM-59 and are not added here. The TypeScript frontend does not synthesize Node, DOM or browser imports/globals.

## Validation contract

Positive and negative fixtures are listed in tests/typescript/manifest.json. Rust unit tests verify erasure and original spans; CLI integration tests compile, inspect and execute representative TypeScript and TSX files through the same Core IR/OGMB/runtime path as JavaScript. Runtime/browser acceptance remains part of the integrated promotion candidate, not this engineering PR.
