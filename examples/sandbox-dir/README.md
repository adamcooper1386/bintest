# Sandbox Directory Example

This directory demonstrates the `sandbox_dir` configuration option for persisting test sandboxes.

## Running These Tests

Run from this directory to pick up the `bintest.yaml` suite configuration:

```bash
# From the repository root:
bintest run examples/sandbox-dir/

# Or from this directory:
cd examples/sandbox-dir
bintest run .
```

## What's Demonstrated

- `bintest.yaml` - Suite configuration with `sandbox_dir: local`
- `test_persist.yaml` - Tests that create files in the sandbox

When `sandbox_dir: local` is set, sandboxes are created in `.bintest/<timestamp>/` instead of the system temp directory, allowing you to inspect test artifacts after execution.
