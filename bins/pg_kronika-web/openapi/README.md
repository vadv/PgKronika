# Generated OpenAPI

This directory is generated from the Rust handlers and response schemas used by
`pg_kronika-web`.

Do not edit these files manually. Run `make openapi` from the repository root
to regenerate and round-trip-check the tree. The entry point is
`openapi.yaml`; path and schema fragments use standard relative `$ref` values.
Run `make openapi-bundle` when a tool requires one self-contained YAML file.
