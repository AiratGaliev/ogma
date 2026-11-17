# Debugger API M0 proof

This folder contains the deterministic interpreter/debugger proof used by OGM-33. It is intentionally independent of the future Rust workspace and has no third-party dependencies.

Run the focused suite from the repository root:

```sh
python -m unittest discover -s .github/scripts -p "test_debugger_contract.py" -v
```

- debugger_api.py defines the backend-neutral facade and immutable result DTOs.
- prototype.py models a small instruction set, owner-thread safe-point control, breakpoints, source-level stepping, call stacks, lexical captures and temporary inspection roots.
- metadata.py projects Core IR v1 source provenance into the existing .ogmb v1 source-map shape using encoder-provided offsets and storage locations.
- dap_adapter.py translates a limited normalized command set without importing a VM or Runtime implementation.

The harness is not an ECMAScript implementation, production Runtime, production GC, native AOT debugger or DAP server. Native/mixed stacks, async continuations, Hot Reload and IDE-client acceptance remain outside the M0 proof.
