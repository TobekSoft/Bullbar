# Prior art, components, gaps and bridges

Checked November 2026. Projects change fast: re-check maintenance status before depending on anything.

The short version: the plumbing (scrubbing proxies, gateways, routers, runners, UIs) exists many
times over. What this search didn't find is anything that treats the AI provider itself as the party
to protect against; if something does, please say so. Bullbar uses the plumbing and builds the
rest.

## Status key

- ✅ exists, use as is (as a sealed component)
- 🔧 exists, needs a bridge
- ❌ not found, Bullbar builds it ("built" where it has, November 2026)

## By layer

| Bullbar needs | Existing pieces | Status |
| --- | --- | --- |
| Gateway / proxy | Bullbar's own endpoint in front; behind it a provider library (async-openai) or a gateway (Bifrost; AISIX to watch). LiteLLM, Plano (ex-Arch), Portkey considered and not used | 🔧 |
| Scrubbing and restore | PrivAiTe (reversible PII proxy; standalone, Open WebUI filter or LiteLLM guardrail), Grepture, Presidio, an open PII model (Apache-2.0, runs via ONNX, callable from Rust) | ✅ |
| Secret scanning | gitleaks, detect-secrets, trufflehog rule sets | ✅ |
| Local models | Ollama, llama.cpp; mistral.rs and candle (Rust) | ✅ |
| Capability routing | RouteLLM (Apache-2.0, strong/weak only), Arch-Router (1.5B, plain-language policies), Not Diamond (commercial) | ✅ |
| Privacy-aware routing | llm-localfirst (binary sensitive flag, fails closed), vLLM Semantic Router (privacy as a routing signal; Kubernetes/Envoy, heavy for home), PRISM (research: cloud / sketch collaboration / local) | 🔧 partial |
| Chat interface | Open WebUI, LibreChat | ✅ |
| Tunnel | WireGuard; Headscale (self-hosted control server) | ✅ |
| Email aliases | SimpleLogin, addy.io, Bitwarden generator | ✅ |
| Secrets / vault | Bitwarden Secrets Manager, OpenBao, SQLCipher, age | ✅ |
| Settings precedence | Cedar (Rust), Open Policy Agent | 🔧 |
| Provider terms monitoring | Open Terms Archive (records every version of services' terms; AI collections), ToS;DR | 🔧 feed into scoring |
| Canaries | Canarytokens | 🔧 built for access detection, not ads |
| Chaff prior art | TrackMeNot, AdNauseam (browser only) | 🔧 design reference |
| Exposure axis routing (two-axis grid) | — | ❌ built |
| Provider terms scoring, chain scoring | — (archives track changes, don't score) | ❌ |
| Exposure ledger | — | ❌ |
| Topic-pinned federation across unlinked accounts | — | ❌ built (pinning; unlinked accounts are up to you) |
| Personas, chaff generator, chaff-reply diversion | — for AI traffic | ❌ |
| Local de-obfuscation (persona correction, decoy stripping) | — | ❌ |
| Shutters orchestration | pieces only | ❌ |

## Caveats found

- **TensorZero** (Rust gateway) was wound down in June 2026 and its repository archived. Read it for
  ideas; don't build on it.
- **Open Terms Archive:** one page for the contributor-run AI collection lists an end-of-tracking date
  of 1 December 2025. Check it is still active; a separate GenAI Governance Archive collection tracks
  major providers.
- **Anonymising relays** appear to be consumer-only with no official API; automating a web chat would
  likely breach terms.
- **Corporate gateways** (PII redaction, DLP) are a crowded market. Bullbar is not competing there;
  their threat model is staff, ours is the provider.

## Build / bridge / adopt

1. **Build (Rust, Bullbar's brain):** exposure grid router, terms scoring, exposure ledger, federation
   and accounts, obfuscator and deobfuscator, shutters, always-on floor.
2. **Bridges:** provider adapters behind Bullbar's endpoint; scrubber results → ledger;
   Open Terms Archive changes → rescoring and demotion; Cedar for settings precedence;
   canary records → shutter triggers.
3. **Adopt as sealed components:** everything marked ✅, each in its own process with its own permissions.

## Sources

- PrivAiTe — https://pypi.org/project/privaite/
- Grepture — https://grepture.com/
- Open-source vs commercial AI privacy tools — https://dev.to/gunxueqiu6/open-source-vs-commercial-ai-privacy-tools-5-options-compared-4o7c
- RouteLLM — https://lmsys.org/blog/2024-07-01-routellm/
- Arch-Router paper — https://arxiv.org/abs/2506.16655
- llm-localfirst — https://pypi.org/project/llm-localfirst/
- vLLM Semantic Router — https://www.llmreference.com/router/vllm-semantic-router
- PRISM — https://arxiv.org/abs/2511.22788
- Privacy Guard & Token Parsimony (routers ignore sensitivity) — https://arxiv.org/abs/2603.28972
- Open Terms Archive — https://opentermsarchive.org/en
- TensorZero status — https://enterprisedna.co/directories/open-source/tensorzero
