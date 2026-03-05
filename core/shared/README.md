# shared

Shared contracts across app and daemon.

Scope:

- XPC schemas and versioning
- shared error taxonomy
- settings and policy models
- compatibility metadata
- shared Rust logging primitives used by daemon/providers

Contract changes must preserve backward compatibility or ship with migration plan.
