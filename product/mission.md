# Mission

Our mission is to make executable integration tests **boring, deterministic, and machine-authorable**.

This tool exists to solve a specific problem:  
**humans and AI agents struggle to write and maintain Rust integration tests for binaries** because the current ecosystem requires too many implicit decisions, ad-hoc harnesses, and custom glue code.

We believe that:
- Integration tests for executables should be **defined, not programmed**
- Setup, execution, and teardown should be **explicit and serialized**
- Tests should be runnable **outside of Cargo**
- A single test definition should be usable by:
  - humans
  - CI systems
  - autonomous AI coding agents

The tool prioritizes:
- determinism over flexibility
- explicit state over shared globals
- declarative configuration over Rust test APIs
- predictable structure over clever abstractions

If an AI agent can read a test file and confidently answer:
> “What will this binary do when run, and how do I know if it worked?”

Then the tool is succeeding.

