# Bullbar

**Bullbar is a personal, privacy-first gateway between you and AI models.** It answers what it can
at home, sends the rest to the cheapest model that's good enough, and strips what identifies you
before anything leaves the house. Every AI provider is treated as untrusted but useful.

> [!WARNING]
> **A proof of concept, not a tool to rely on.** Bullbar is an experiment, put out to get its ideas
> criticised. Please don't trust it with anything you can't afford to have leave your machine.
>
> - **How it was built:** with an AI coding assistant, by someone new to Rust and to data privacy.
>   The tests and the design rules are conservative; the person behind them is learning.
> - **What it misses:** no detector finds everything, and Bullbar's don't. On a small synthetic set
>   written while building it (so expect worse on real text), the scrubber found 108 of 116 personal
>   details, and only 11 of 19 relationships ("my wife", "my boss"). It also replaces some things
>   that aren't personal: today's date in a chat client's system prompt is shifted like a personal
>   date, so answers that depend on the date may be off. Scrubbing by a local model, which would catch relationships and indirect
>   details, isn't built. Bullbar doesn't rate your requests yet: you do, by the model name you pick
>   (unrated work counts as Sensitive and stays home). The rest of what isn't built is listed under
>   [Status](#status), and `bullbar serve` names it at start.
> - **No warranty, no promise of maintenance.** It is provided "as is", under the licences below.
>   Not legal advice.

## The design, for developers

The shape, in the usual pattern names: an **API gateway** (one OpenAI-style front door for every
chat client), a **policy decision point** (a pure-logic decision engine) apart from the
**enforcement points** that carry its decisions out, a **content-based router** on two axes,
**pipes and filters** on the way out and back (strip credentials, scrub, check what leaves;
restore, footer), **pseudonymisation** of personal details, **ports and adapters** for its parts
(slots, each with a safe behaviour when empty), and **fail closed** throughout.

- [How Bullbar works](docs/how-it-works.md): one request's path through the code as built, step by
  step, with what can stop it and where each step lives.
- [Concept design](docs/concept-design.md): the whole design, most of it not built yet.
- [Prior art, components and gaps](docs/prior-art.md): what exists elsewhere, and what Bullbar uses.

Code comments cite decision records (ADR nnnn). They are kept private for now.

### Layout

```
crates/bullbar-core/      # the decision engine: types, policy, slots, decisions; pure logic, no I/O
                          # apart from reading config
crates/bullbar-gateway/   # Bullbar's endpoint (async; axum, async-openai): the floor, scrubbing,
                          # restoring, streaming, the control channel, trace
crates/bullbar-cli/       # the `bullbar` command: serve, the control commands, and the dry-run tools
                          # (decide, check-config, scenarios, scrub)
config/                   # sample setups: casual, paranoid, corporate, gateway-only
scenarios/                # hand-written decisions the engine is checked against; pii/: the synthetic
                          # test set for the scrubber
Containerfile             # Bullbar's image (distroless, non-root)
compose.yaml              # Bullbar and Ollama for `podman compose`; the runner has no way out
compose.scrub.yaml        # opt-in: mounts the scrubber's entity model and serves with it
container/                # the container setup's accounts file
scripts/                  # fetch-pii-model.sh: the entity model, pinned and hash-checked
docs/                     # the design, how it works, prior art, running with Jan
```

## Status

**Built** (as of November 2026):

- **The decision engine, on both axes.** How sensitive a request is sets the most exposed place it
  may go; how hard it is sets the least capable model that will do; the engine picks the cheapest
  option inside both and explains every decision. Both axes work today, on ratings **you give**
  through the model name (`bullbar/personal/hard/health`). A classifier would only fill them in for
  you.
- **The always-on floor:** credentials stripped, an independent check on everything leaving, a kill
  switch. A protective part that fails blocks the request.
- **Scrubbing:** personal details replaced with realistic stand-ins on the way out and put back in
  the answer (patterns plus an open entity model, run on the CPU). Credentials never come back.
- **Topic pinning:** each category of personal work goes to one account, so no single provider
  sees it all.
- **Questions, not dead ends:** when a decision is yours (one step more exposed, or settling for the
  local model), Bullbar asks on a control channel.
- **`trace`:** see exactly what leaves, for a few requests.

**Not built yet:** automatic rating (the biggest missing piece), judging answers, scoring provider
terms (each account's place is set by hand), the exposure ledger, personas and decoy traffic,
canaries, the shutters beyond a kill switch, the anonymous relay row, mobile access.

## Try it

Bullbar runs in containers (Podman), beside a local model runner that has no way out, and works
with any chat client that takes a custom OpenAI-style endpoint. **Step by step on Linux, with Jan as
the chat client:** [Running Bullbar with Jan](docs/running-with-jan.md). It covers the prerequisites,
setup, scrubbing, a cloud account, the settings, and `trace` to see exactly what leaves.

**Tested in November 2026** with Ollama running `llama3.2:3b` locally, and Groq's OpenAI-compatible
API as the cloud account. Any provider with an OpenAI-style chat API should work, and a small local
model of your choice. Naming them is not a recommendation: Bullbar treats every provider as
untrusted.

To try the engine alone, without containers or any network:

```bash
cargo run -p bullbar-cli -- decide --config config/casual --sensitivity personal --complexity hard
cargo run -p bullbar-cli -- check-config --config config/paranoid
cargo run -p bullbar-cli -- scenarios
echo "my sister Jane Doe lives at 12 Example Road, call 07700 900123" | cargo run -p bullbar-cli -- scrub
```

`config/` holds four sample setups (casual, paranoid, corporate, gateway-only); `scenarios/` holds
the hand-written decisions the engine is checked against. `scrub` uses patterns alone unless given
the entity model (`--model`, see the Jan guide): without it, the example above leaves with the
address and phone number replaced but the name and the relationship still there.

## What it is meant to do

Built so far: local first, two-axis routing, scrubbing, federation by topic, a kill switch. The rest
is the design.

- **Local first.** If a local model can answer, the request never leaves.
- **Two-axis routing.** How hard a request is sets the minimum model capability; how sensitive it is
  sets the maximum exposure. Bullbar picks the cheapest option inside both.
- **Scrubbing.** Credentials, personal details and relationships are removed on the way out. Personal
  details are put back in the answer; credentials never are (they stay as placeholders).
- **Provider terms scoring.** A provider's place on the exposure axis is earned from its contracts,
  re-checked when they change, and demoted automatically until reviewed.
- **Exposure ledger.** Tracks what each provider has already seen, and tightens as pieces accumulate.
- **Federation.** Topics pinned to different providers, so none holds the whole picture.
- **Countermeasures (optional).** Personas and decoy traffic, so profiles built on you are noise.
- **Shutters.** When something looks wrong, Bullbar closes. Reopening needs a person.

## What it isn't

Not a corporate data-loss-prevention tool. Those protect a company from its own staff.
Bullbar aims to protect a person from the provider.

## FAQ

**Is this new?**
Others can judge. The plumbing (scrubbing proxies, gateways, routers, local runners) exists many
times over; [prior art](docs/prior-art.md) lists what was found in November 2026. If something
already does what Bullbar sets out to do, please say so.

**Does it work with the official consumer AI apps?**
No. Those apps don't route through a proxy. Bullbar works with API access and with any app that
lets you set a custom endpoint.

**Is it legal to use?**
Scrubbing, local-first and routing are ordinary data handling. Some countermeasures may conflict
with some providers' terms; they're optional and used at your own risk. Not legal advice.

## Licence

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
