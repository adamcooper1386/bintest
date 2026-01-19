# Suite Configuration Example

This directory demonstrates suite-level configuration via `bintest.yaml`.

## Running These Tests

These tests must be run from this directory (not from the parent `examples/` directory) so that bintest can find the `bintest.yaml` suite configuration file:

```bash
# From the repository root:
bintest run examples/suite-config/

# Or from this directory:
cd examples/suite-config
bintest run .
```

Running from the parent directory (`bintest run examples/`) will cause these tests to fail because the suite-level environment variables won't be available.

## What's Demonstrated

- `bintest.yaml` - Suite-level configuration with timeout, environment variables, and setup/teardown
- `test_env.yaml` - Tests that suite-level environment variables are available
- `test_override.yaml` - Tests that file-level settings override suite-level settings
