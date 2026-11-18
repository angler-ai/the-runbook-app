# Licensing

The Runbook is open source, owned by Angler AI, Inc., in two parts.

| Part | License | Where |
| --- | --- | --- |
| The server, web app, CLI, MCP server, sign-in service, Cloudflare edition, deploy files and scripts | [AGPL-3.0-only](LICENSE) | Everything not listed below |
| The runbook language (parser, compiler, expression language), the API data types and OpenAPI contract, the JSON Schema, the Claude Code plugin and agent skill, and the examples | [Apache-2.0](crates/runbook-spec/LICENSE) | `crates/runbook-spec`, `crates/runbook-expr`, `crates/runbook-model`, `crates/runbook-contract`, `schemas/`, `api/`, `plugins/`, `skills/`, `examples/` |

**What this means in practice**

- **Using The Runbook**, self-hosted or not, places no obligations on your runbooks, your agents or the systems they act on. Running the CLI or the server as they are is just use.
- **The Apache-2.0 parts** may be embedded anywhere, including closed-source tools: write your own client, validate runbooks in your CI, build an editor plugin.
- **If you modify the AGPL parts and offer them to others over a network**, you share those modifications under the AGPL (section 13).
- **The CLI is AGPL for now** because it includes server code (local runs, reports). Splitting it so it can be Apache-2.0 is planned.

**Contributing.** Contributions are accepted under a contributor license agreement with Angler AI, Inc., so the project can be relicensed if needed. See [CONTRIBUTING.md](CONTRIBUTING.md).

Third-party code keeps its own license: `vendor/cel-core` is MIT OR Apache-2.0.
