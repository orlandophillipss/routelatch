# RouteLatch

A small Rust API that turns typed Jev decisions into replayable workflow-routing receipts. Model judgment selects a candidate; ordinary Rust code checks the allowed options, confidence threshold and integer cost budget. The service returns a route or review decision. It never executes arbitrary tools.

## Run

```sh
cargo test
cargo run --release
curl http://127.0.0.1:8082/health
curl -H 'Content-Type: application/json' --data-binary @examples/evaluate.json http://127.0.0.1:8082/v1/evaluate
```

`POST /v1/evaluate` works offline with a supplied decision. It validates the probability map, selected option and policy, then hashes the canonical serialized evaluation with SHA-256. Workflow maps are sorted, so JSON key ordering does not change a receipt. The fingerprint identifies the policy and decision, not the original state and not a cryptographic signature.

`POST /v1/route` accepts `{ "state": "Find documentation", "policy": ... }`, calls the [official TypeSafe endpoint](https://docs.typesafe.ai/introduction/quickstart) with `jev-latest`, and applies the same gates. Set `TYPESAFE_API_KEY` in the server environment. Missing keys return 503; provider failures return 502. Confidence and probability are separate inputs: confidence is taken from the provider, not inferred from the highest probability. No live model output is simulated when a key is absent.

## Boundaries

Axum + Tokio, bounded request and provider-response sizes, a reusable HTTP client and a 12-second upstream timeout. Localhost by default. `ROUTELATCH_ADDR` changes the binding. Add authentication, quotas and deployment-specific observability before exposing it to untrusted clients. There is no durable queue, receipt database or claimed production capacity.

Tests cover routing, confidence and budget gates, malformed distributions, receipt replay, HTTP behavior, missing credentials and a local mock provider using the documented Jev request/response shape. A live paid-provider call is not part of the test suite. No benchmark claim is made until a measured workload is recorded.

MIT licensed.
