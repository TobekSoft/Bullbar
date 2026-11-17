# Bullbar — Concept Design

Nov 15, 2026 · Tobeksoft

> **A proof of concept, not a tool to rely on.** This document is the design Bullbar is heading for,
> not a description of what it does today. Bullbar is an experiment, put out to get its ideas
> criticised. It was built with an AI coding assistant, by someone new to Rust and to data privacy.
> What is built, and the path a request takes through it, is in [How Bullbar works](how-it-works.md).
>
> **Built:** the decision engine on both axes (you give the ratings, through the model name you pick;
> unrated work counts as Sensitive and stays home); Bullbar's own chat endpoint; the always-on floor
> (credential stripping, the outbound check, a kill switch); scrubbing of personal details with
> patterns and an entity model, restored in the reply; topic pinning across accounts; a local runner
> and one cloud adapter; Bullbar's questions on a control channel.
>
> **Not built yet:** the classifier (automatic ratings, the biggest missing piece), the answer check,
> the terms register (each account's row is set by hand), the exposure ledger, scrubbing by a local
> model (context: relationships and indirect identifiers), obfuscation (personas, decoy content, chaff),
> canaries, the shutters beyond a kill switch, the relay row, mobile access.

## Purpose and goals

A personal gateway that sits between you and every AI model, so each request goes to the right model and leaves the house carrying as little about you as possible.

**How it started:** watching footage from a recent conflict that mentioned active countermeasures; thinking about AI, privacy and enshittification; carelessly sharing credentials with an AI agent in a copy-and-pasted fstab file and realising leaks are permanent; not to mention an extra glass of wine with dinner. Chaff, decoys, shutters and canaries come from there. Bullbar considers release of data itself as an issue of least privilege, not just the secrets, and explores these ideas as a proof of concept.

Today every prompt goes straight to a provider, in full, tied to your account, card and home IP. The provider's retention and training terms decide what happens next. The gateway takes that decision back.

- **Efficiency:** anything a local model can handle is answered at home and never leaves the house; cheap models take the next tier, and only hard work pays premium prices.
- **Privacy:** no single provider sees enough to build a useful profile of you.
- **Security:** credentials and Secret-rated content never leave the machine, and the gateway itself keeps as little as possible.

The design rule behind all of it: **anything that knows who you are stays home; the cloud only ever handles content that is already clean.**

The second rule: **build for a moving target.** Models, prices, terms, APIs, runtimes and hardware will all change, often without notice.

- **Config, not code.** Providers, models, rows, columns and thresholds live in configuration; nothing is hard-wired to a name.
- **One adapter per provider.** A provider changing its API breaks one adapter, not the gateway.
- **Detect change.** Scheduled checks of terms pages, model lists, prices and endpoint health.
- **Fail safe.** Anything that changes unreviewed is demoted (treated as a more exposed row, or taken out of rotation) until a person looks at it. A change never loosens anything by itself.

The third rule: **least privilege everywhere.** Every component, key, person and provider gets only the access its job needs, so one compromised part exposes one part.

| Component                 | Can                                                          | Cannot                                                      |
| ------------------------- | ------------------------------------------------------------ | ----------------------------------------------------------- |
| Scrubber                  | Read plain requests, write placeholder maps, restore replies | Reach the network or use tools                              |
| Local model runner        | Read the prompt it is given                                  | Reach the network or the vault                              |
| Deobfuscator              | Read replies and the request's record of what was added      | Reach the network, use tools, or write anything that leaves |
| Vault                     | Store maps, personas, ledger                                 | Be read by anything but the scrubber, restorer and router   |
| Cloud layer               | Relay scrubbed requests                                      | See plain text, mappings or the topic map                   |
| Providers                 | Receive one scrubbed request                                 | Receive more than that request needs                        |
| API keys                  | Call one provider, within a spend limit                      | Be shared across providers or tiers                         |
| Phone and desktop clients | Send requests with their own revocable token                 | Read settings, the ledger or other clients' history         |
| Agent tools               | Use the tools a task needs, for that task only               | Keep standing shell, file or network access                 |
| Users and admins          | Change settings within their level                           | Loosen the level above, or read others' requests            |

- **Data minimisation is least privilege for content.** Scrubbing is the same principle applied to what a provider is allowed to know.
- **Containers run non-root**, with read-only filesystems and network rules matching the table.

The intent behind the rules: **firm on what leaves, honest about the setup, flexible within policy**.
What makes Bullbar worth having never bends at any tier: Secret and credentials stay home,
nothing personal leaves unscrubbed, every decision is explained, nothing is bypassed silently. A weaker
setup says plainly what it does less, once, rather than looking like a strong one. Everything else is a
default the user can change knowingly, within what an admin allows. The best protection is a full local
layer, but not everyone will build one, and for them the alternative is the provider's own app.

**No finder is complete** (2026-11-16, from measuring the scrubber). Every detector Bullbar
uses (patterns, an entity model, a local model) misses something, at every tier. The firm rules above
hold for what Bullbar knows: a Secret rating, a credential the scanner finds, a detail the scrubber
found never leaves. What the finders miss, they miss, so Bullbar layers them (each layer only adds
finds), measures them on test sets nobody tuned them for, says what it doesn't cover, judges each gap
by what it would cost (who you are before who you know), and learns from misses in use, keeping stats
and patterns, never the content (planned). Roo Spotter, later, would judge leaks by
their consequences.

## Core architecture

Two layers: a local layer that knows who you are, and a cloud layer that only ever sees clean content. Every request runs classify, scrub, route, send, restore, forget.

On the way back, the local layer restores placeholders and corrects for the persona, and nothing in the vault outlives the conversation.

In the usual pattern names: an **API gateway** (one front door for every chat client), a **policy decision point** (the decision engine, pure logic) apart from the **enforcement points** that carry its decisions out, a **content-based router** (the route depends on the request's ratings), **pipes and filters** on the way out and back, and **ports and adapters** for the slots below. [How Bullbar works](how-it-works.md) maps each to the code as built, with a sketch of the path.

### Composition: slots, contracts and components

Bullbar is a composition of interchangeable components, not a host app with plugins: what is fixed is the set of slots, the contract for each slot and the safety invariants, not any particular code.

| Slot                          | Required? | If empty                                                                               | Example fillers                                                                          |
| ----------------------------- | --------- | -------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Decision engine               | Required  | Won't start                                                                            | Bullbar-native (Rust)                                                                    |
| Outbound check                | Required  | Won't start                                                                            | Bullbar-native (Rust)                                                                    |
| Shutters                      | Required  | Won't start                                                                            | Bullbar-native (Rust)                                                                    |
| Policy and settings           | Required  | Won't start                                                                            | Bullbar-native; Cedar or OPA as the evaluator                                            |
| Credential scanner            | Required  | Won't start                                                                            | Bullbar-native rules, gitleaks or detect-secrets rule sets; finds and strips credentials |
| PII and relationship scrubber | Optional  | Sensitive and personal requests capped at local rows                                   | Presidio, an open PII model via ONNX, PrivAiTe                                           |
| Classifier                    | Optional  | Manual ratings only; unrated treated as sensitive                                      | Rules, RouteLLM, Arch-Router                                                             |
| Local model runner            | Optional  | No local rows; sensitive requests leave only scrubbed, or are refused where turned off | Ollama, llama.cpp, mistral.rs                                                            |
| Provider adapters             | Optional  | No cloud rows                                                                          | One per provider account                                                                 |
| Exposure ledger               | Optional  | No history-based tightening; static ceilings only                                      | Bullbar-native                                                                           |
| Terms register                | Optional  | Every provider treated as standard terms                                               | Bullbar-native, fed by Open Terms Archive                                                |
| Federation and accounts       | Optional  | One account per provider, no topic pinning                                             | Bullbar-native                                                                           |
| Obfuscator                    | Optional  | No obfuscation. Personas and decoy content also need a Deobfuscator that can undo them | Bullbar-native                                                                           |
| Deobfuscator                  | Optional  | Replies restored but not de-obfuscated, so no personas or decoy content                | Bullbar-native                                                                           |
| Canaries / Roo Spotter        | Optional  | No leak detection                                                                      | Bullbar-native, Canarytokens                                                             |
| Answer check                  | Optional  | Only the user judges answers                                                           | A small local judge model, tests for code                                                |
| Interfaces                    | Optional  | Command line only                                                                      | Open WebUI, LibreChat, a Bullbar dashboard                                               |

- **A thin composer, not a host.** It wires components together, grants permissions and checks that required slots are filled. It has no features of its own.
- **Contracts, versioned and small.** Each slot has a documented interface: what it receives, what it returns, what it must never do. Any language can fill it.
- **Manifests and permissions.** Each component declares what it needs (network, which data); the composer grants only that. A detector asking for network access is refused at install.
- **Empty means safe, not broken.** The decision engine knows which slots are filled and healthy, and caps routing accordingly.
- **Slots depend on each other**. A mechanism may need another slot (persona correction needs a trusted local runner). The engine works out what each slot can actually do from what is filled, and a missing dependency only removes capabilities. The outbound check never depends on the scrubber; required slots never depend on optional ones.
- **Editions are compositions.** Casual: required slots + scrubber + local runner. Paranoid: add federation, obfuscation and canaries. Corporate: leave out obfuscation, add audit components.
- **Formalise late.** A slot's contract is frozen only once two real implementations exist; until then it is a clean module boundary.
- **Mind the hot path.** Required safety slots run in-process with the decision engine; optional components are called once per request, not per streamed chunk.

## Layer 1: Triage and routing

Every request is scored on two axes, how sensitive and how hard, and **sensitivity always wins** when they disagree.

|                          | Easy                                             | Hard                                             |
| ------------------------ | ------------------------------------------------ | ------------------------------------------------ |
| **Not sensitive**        | Cheap or free cloud model                        | Premium cloud model                              |
| **Sensitive**            | Local model                                      | Best local model, or scrub then premium cloud    |
| **Contains credentials** | Local only, or block (strip-and-send if enabled) | Local only, or block (strip-and-send if enabled) |

- **Cheap classifier first.** Rules and keywords, then a small local model. If triage costs as much as the answer, it is a toll booth.
- **Fail cautious.** An easy job sent to an expensive model wastes money; a private job sent to a public model cannot be undone. Tune the thresholds for the second.
- **User override.** A per-request tag ("local only", "send anywhere") beats the classifier.

**Local first.** The default route is the local model, not the cloud. If it can answer, the request ends there: no scrubbing, no persona, no network call, nothing to restore. Only what the local model can't handle moves on to scrubbing and the cloud.

- **Escalate on doubt.** The local model hands off when it isn't confident, the task is beyond its size, or it needs current web data; the request then continues down the scrub path.
- **Hybrid split.** For mixed tasks, the local model does the private part (summarising a personal document, say) and sends only a clean, derived question to the cloud.
- **Measure the share.** Track what fraction of requests the local model answers; that number decides whether Tier 2 hardware is enough.

### Two axes: capability and exposure

Complexity and privacy are separate axes: complexity (Easy, Medium, Hard) sets the minimum capability a request needs, through a mapping to columns kept in config, privacy sets the maximum exposure it can tolerate, and the router picks the cheapest option inside both limits.

**Minimum viable response.** The goal for each request is the cheapest, least exposed answer that is good enough, not the best answer available. "Good enough" is the check at each cell, and its bar is a setting (per category, per user, per request).

- **Show the source.** Every answer says which cell produced it: local, relay, or which paid tier.
- **One rung further, once.** If a scrubbed answer is poor, the user can retry that one request one row further up the exposure ladder, still scrubbed, and again one more row on the next retry. Never past the level above's limit or a locked category, never for Secret or credentials, and a policy can forbid it. On their own machine a user can bypass Bullbar entirely: a frustrating Bullbar protects nothing.
- **Upgrade on demand.** An "upgrade" action re-runs the request one column right. If the first answer is clean, it goes along as a draft to refine, which is usually cheaper than starting over.
- **Learn the bar.** Upgrades and thumbs-downs per category tune where the router starts next time.

| Exposure (who sees it)                             | Small                        | Mid                                | Top                           |
| -------------------------------------------------- | ---------------------------- | ---------------------------------- | ----------------------------- |
| **Nobody** (local)                                 | Small local model (3–8B)     | Large local model (30–70B, Tier 3) | Not available locally         |
| **Content, not you** (anonymous relay)             | Anonymising relay, free tier | Anonymising relay, paid tier       | To check                      |
| **Provider, no training, short or zero retention** | Small paid API               | Mid paid API                       | Top paid API (frontier model) |
| **Provider, standard terms**                       | Free or consumer tiers       | Consumer apps                      | Consumer apps                 |

Cache and tools sit in front of the grid: free, private, and always tried first.

**Routing rule**

1. Sensitivity sets the most exposed row allowed; complexity sets the smallest column that will do.
2. Pick the cheapest cell inside both limits.
3. **Escalation moves right only**, within the allowed rows, when a cell fails its check: a more capable column, at the cheapest cell inside the same ceiling; the row may change within it, the ceiling never rises.
4. **Only the scrubber moves a request down the rows.** Scrubbing lowers sensitivity, which opens more exposed rows. A failed answer never makes a request more exposed by itself; only the user's confirmed one-off step does (below).
5. **Hard and private (no cell allowed):** scrub to open rows; split so the local model does the private part and the cloud gets a clean derived question; settle for the best local answer, labelled as such; or ask you.

**Rules**

- **Predict, then climb.** The router picks the starting column; climbing every step on a known-hard task wastes time and tokens.
- **A check at every cell.** Something decides "not good enough" (a screener's verdict can only lead right; only the user's can also open a rung): the model's own confidence, a small judge model, tests passing for code, or your thumbs-down.
- **Pass up the question, not the wreckage.** The next cell gets the clean request; a failed draft goes along only if it is clean and useful for critique.
- **Cells are classes, not names.** Model names change every few months; the grid lives in config.
- **Relay row caveat:** the best-known anonymising relays appear to be consumer-only, with no official public API, and automating a web chat would likely breach its terms. That row may need a manual hand-off to the relay's own app, or an anonymising service that does offer an API. To verify.

### Sensitivity ratings and fast paths

Every request carries a sensitivity rating, set by the classifier or by you, and the rating decides which mechanisms switch on, so low-risk requests take a fast, cheap path.

| Rating    | Example                                      | Mechanisms engaged                                                 |
| --------- | -------------------------------------------- | ------------------------------------------------------------------ |
| Public    | "Convert 5 miles to km", a recipe            | Always-on floor only; cheapest viable cell                         |
| Personal  | A question mentioning family, place or plans | + PII and relationship scrubbing, restore, topic pinning           |
| Sensitive | Health, legal, finance, work code            | + personas, chaff cover, exposure ledger, local first, row ceiling |
| Secret    | Anything you mark secret                     | Local only; nothing leaves                                         |

- **Credentials are separate from ratings.** A request containing credentials stays local or is blocked, unless strip-and-send is enabled; the credentials themselves never leave.
- **The always-on floor.** Deterministic credential scan, the independent outbound check, shutters and logging rules run on every request, whatever its rating. They are cheap and they catch mistakes, including yours.
- **Raise freely, lower with care.** You can rate any request higher. Rating it below the classifier's suggestion asks for confirmation and stays within org limits.
- **Unrated means classified.** No rating given, the classifier decides. Until it is built, unrated means Sensitive.
- **Fast paths can't break personas.** Unprotected public requests go through separate accounts from persona accounts, or still get the persona's framing, so a stream of real-identity traffic on a persona account doesn't give the game away.

### Settings: admin, user, request

Both axes are adjustable at three levels, and each level can move a setting only within the bounds of the level above. The level above the user is the **admin**: an employer, a parent, a professional duty, or your own earlier self.

| Setting                       | Controls                                                                          | Admin                                                   | User                                       | Per request                                                                    |
| ----------------------------- | --------------------------------------------------------------------------------- | ------------------------------------------------------- | ------------------------------------------ | ------------------------------------------------------------------------------ |
| Exposure ceiling per category | Most exposed row allowed for health, legal, finance, work code, personal, general | Sets hard limits, can lock (a locked category is firm)  | Tightens or loosens within admin limits    | Tighten freely; one-off exception one rung further, unless forbidden or locked |
| Capability ceiling            | Highest column allowed                                                            | Sets the cap                                            | Lowers or raises within the cap            | Can ask for a higher column within the cap                                     |
| Budget                        | Spend per day or month, per column                                                | Sets the cap                                            | Sets a personal cap below it               | Shown before a costly call                                                     |
| Escalation                    | How eagerly a request climbs: confidence threshold, auto vs ask                   | Default                                                 | Adjusts                                    | "Try harder" or "good enough"                                                  |
| Scrubber modes                | Block, warn and ask, or auto-redact per content type                              | Can forbid strip-and-send for credentials               | Chooses within what is allowed             | Approve or reject a warning                                                    |
| Category labels               | What counts as health, legal, work, and so on                                     | Defines shared categories (client names, project codes) | Adds personal ones (family names, address) | Tag a request manually                                                         |

- **Precedence:** admin hard limits beat user settings, and user settings beat per-request choices. Tightening is always allowed at any level; loosening only within the limit above.
- **Some things are never adjustable:** credentials and Secret-rated content never leave the local layer, whatever the settings.
- **Changes are logged without content:** who changed which setting, when, from what to what.
- **A single-user install is its own admin:** the admin level is just a second, harder-to-change layer of your own defaults.

### Exposure history

The limits also adjust themselves, based on what each provider has already seen: risk is cumulative, so the router judges each request by what it adds, not on its own.

- **Exposure ledger.** The vault keeps a coarse record per provider: categories seen, counts, and hashed entity tags (not content).
- **Tighten as pieces accumulate.** Each related request makes linking easier (condition + suburb + age). Past a threshold per topic per provider, the router tightens that topic: more scrubbing, a different provider, local only, or more chaff.
- **Loosen only where it adds nothing new.** If a provider already holds a fact at a given level of detail, a request at the same level adds little and may route there. New specifics are still checked, so "it's already out" can't ratchet everything open.
- **Decay with retention.** Exposure fades on the provider's stated retention period, so a zero-retention provider builds little history and a 30-day one forgets after 30 days.
- **Feeds the other layers.** The ledger drives topic pinning (federation) and chaff volume (obfuscation).
- **What each provider knows.** A view showing, per provider, the categories and topics it has seen and when that fades.
- **The ledger is itself a profile.** It stays in the vault, coarse, encrypted and decaying, and is never sent anywhere.

### Provider terms triage

A provider's row on the exposure axis is earned from its contracts and policies, not assumed, and is re-checked whenever those change.

| Criterion                  | What to check                                                                | Effect on routing                                          |
| -------------------------- | ---------------------------------------------------------------------------- | ---------------------------------------------------------- |
| Training use               | Are prompts used to train, by default or at all? Opt-out or opt-in?          | Training by default drops it to the standard-terms row     |
| Retention                  | How long prompts and outputs are kept; zero-retention available?             | Sets the row, and the decay period in the exposure ledger  |
| Safety-review exceptions   | Is flagged content kept longer or read by people, even under zero retention? | Sensitive categories avoid providers with broad exceptions |
| Jurisdiction and residency | Where data is processed and stored; which country's law applies              | Org can require a region; affects legal-access risk        |
| Legal requests             | Transparency reports; whether users are notified of government requests      | Weighs on the most sensitive categories                    |
| Subprocessors              | Who else touches the data (cloud hosts, contractors)                         | More parties, lower score                                  |
| Breach notification        | Commitments and timelines                                                    | Tie-breaker                                                |
| Change notice              | How much notice before terms change                                          | Short notice means more frequent re-checks                 |
| Account type               | Consumer, API and enterprise terms often differ for the same provider        | Each account is scored separately                          |

- **Terms set the row; capability sets the column.** The same model can sit in different rows depending on which account and terms it is reached through.
- **Watch for changes.** Terms pages are re-checked on a schedule; any change demotes the provider one row until reviewed.
- **Org minimums.** The org can require, for example, no training, at most 30 days' retention and a named region for work categories.
- **Scoring is assisted, the decision is human.** A model can summarise and flag terms, but a person approves each provider's row.

**Score the chain, not the brand.** Many services are wrappers: the company you sign up with, the model maker behind it, and the cloud host underneath each have their own terms. A request is only as private as the weakest link in that chain, and a wrapper that switches its underlying model can change its score overnight. Two services with similar capability, such as a model maker's own API and a wrapper product built on several makers' models, can therefore land in different rows.

### Side channels and canaries

Bullbar protects AI traffic only; data also leaks through accounts, payments, browsing and apps, and comes back as targeted ads, so the design adds account hygiene and uses the ads themselves as a leak detector.

**Account hygiene that Bullbar depends on**

- **A separate email alias per provider account** (Bitwarden's username generator can create forwarding aliases), never your main address.
- **No "Sign in with Google/Apple/Facebook"** on provider accounts; it links the account to an advertising identity.
- **Virtual or separate payment cards** per provider, where available.
- **Provider web consoles opened in a separate, tracker-blocking browser profile**; the API itself carries no ad trackers, the dashboard may.
- **Your own follow-up behaviour.** Asking Bullbar privately, then searching the same thing on a tracked search engine, undoes the work. Point searches at a private engine.

**Canary tests**

- **Plant a unique topic with one provider only.** A made-up interest (a niche hobby, an obscure product) sent as chaff through a single account, never searched or mentioned anywhere else.
- **Watch the ads.** If that topic starts appearing in social media or web ads, something in that provider's chain, or the account around it, is feeding ad networks.
- **Treat a hit as a signal, not proof.** Ad systems infer from many sources. Rotate canaries, run several at once, and confirm before demoting a provider.
- **A confirmed hit is a shutter trigger:** that provider drops to the standard-terms row or out of rotation.

**Scope.** Automated side-channel monitoring (watching ad feeds, recognising canary topics in them, correlating hits) is a feature set of its own, with its own access and terms problems. It is out of the core build and deferred to an optional later module, *Roo Spotter*. Until then, Bullbar only plants and records canaries; you watch for them by hand.

## Layer 2: Scrubbing (subtraction)

Scrubbing removes what identifies you from each request and puts it back in the reply; it only ever subtracts, never invents.

1. **Deterministic detection first.** Pattern and entropy rules (the kind secret scanners such as gitleaks use) catch credentials (API keys, tokens, passwords, card numbers) reliably.
2. **PII detection second.** Names, addresses, phone numbers, emails, dates of birth (Microsoft Presidio is the usual starting point).
3. **Local LLM pass last.** Catches the subtle cases, such as a paragraph that describes a medical condition. It is probabilistic, so it is never the only guard on credentials.

**Relationships are sensitive data too.** "My wife", "my son", "my boss" map your private life onto an identifiable account. "My wife who has lupus" becomes "a woman who has lupus": the medical question is unchanged, the link to your household is gone.

**Placeholder swapping.** "John Smith" becomes PERSON\_1 on the way out and is restored on the way back. Replies must be checked: models rephrase, merge or invent placeholders.

**Modes per content type:** block, warn and ask, or auto-redact.

**Throwaway mappings.** The placeholder map lives in memory for one conversation, then is deleted. No permanent decoder ring.

## Layer 3: Obfuscation (personas, decoy content, chaff)

Scrubbing removes; obfuscation adds. Scrubbing protects each request; obfuscation protects the aggregate, by feeding each provider's profile of you plausible but false data. The approach is described in Brunton and Nissenbaum's *Obfuscation*.

Two slots, split by direction: the **Obfuscator** adds on the way out, the **Deobfuscator** undoes it on the reply. The Obfuscator only adds what the Deobfuscator can undo: no trusted local runner means no persona correction, so no personas.

**Personas.** Each provider sees a consistent invented identity. "My patient who has lupus" changes the data the provider aggregates, independent of the answer.

**Chaff.** Decoy requests on topics you don't care about, so the real cluster stops standing out. The topic is the signal, the framing only a label: ten lupus questions say "lupus near this account" whatever the persona.

**Decoy content.** Plausible but irrelevant material inside a real query (decoy questions or context), so the query itself says less about what you want. Recorded per request in the vault like surrogates, never resembling a real person's surrogate, and stripped from the reply by the Deobfuscator. Costs tokens on every real request and may affect the answer; weighed when built.

**Local correction.** The gateway knows the truth, so the reply is re-read locally and recalibrated (caveats added back, expertise assumptions removed). The provider gets the costume; you get the real answer.

Rules:

- One or two **long-lived** personas per provider, consistent over months. A new disguise every request is easy to filter.
- Chaff must be **plausible for the persona**. A "doctor" asking about lupus, then gout, then a rash is believable; lupus then sourdough is not.
- Generate chaff with **local models** (free), and send it mostly to cheap or free endpoints, since paid chaff burns tokens for nothing.

**Handling chaff replies.** Dummy queries get real answers back, and those must be separated from real traffic without the provider being able to tell the difference.

- **Tagged only at home.** Each request ID is marked real or chaff in the vault. Nothing in the request itself (headers, wording, timing) marks it as chaff.
- **Same shape on the wire.** Chaff uses the same account, client, headers and timing patterns as real traffic, or it filters itself out for the provider.
- **Diverted on return.** Core diverts chaff replies by tag to the Obfuscator, never to the user, the answer cache or the restorer. Diversion is not a slot and fails closed: a chaff reply reaching you is a fault.
- **Used, then dropped.** The Obfuscator may read a chaff reply to write a plausible follow-up (real users ask follow-ups), then discards it.
- **Never cached.** An answer to a fake question must never be served later as a real answer.
- **Separate accounting.** Chaff counts toward cost and cover, not toward the exposure ledger's real-topic counts.
- **Still untrusted.** Chaff replies are sandboxed like any other inbound text.

## Layer 4: Federation (spreading the pieces)

Each request goes out whole, but the stream of requests is spread across providers, so no single provider holds the whole jigsaw.

- **Whole requests, not fragments.** Cutting one question into pieces breaks the task; spreading complete questions keeps answer quality intact.
- **Pin by topic, not at random.** Health always to provider A, legal always to B, code to C. Random spreading gives every provider a sample of your whole life; topic pinning gives each a deep but narrow slice with none of the cross-links (health + legal + postcode) that identify a person.
- **Stay pinned.** Lupus to A on Monday and B on Thursday means both know about lupus.
- **Unlinked accounts.** Separate accounts, payment and egress IP per provider where possible. Linked accounts federate nothing.
- **Optional API router in the middle.** The provider sees the router, not you, but the router sees everything: it moves trust rather than removing it.

## Inbound: restore, de-obfuscate, stitch

The return path is feasible in three steps of falling reliability: restoring placeholders is mostly mechanical (the PII scrubber), de-obfuscating is moderate (the Deobfuscator: correcting for the persona, stripping answers to decoy content), and stitching split work back together depends on how the work was split.

| Step                 | Feasibility          | Main problems                                                                                                                               | Approach                                                                                                                                                                                                                  |
| -------------------- | -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Restore placeholders | High                 | Models reword tokens ("Person 1", "PERSON\_1's"), merge or invent them; grammar must agree (he/she, a/an, plurals)                          | Realistic surrogates instead of tokens (a consistent fake name of the same gender, dates shifted by a fixed offset, an address in the same region); validate every surrogate comes back once, flag extras or missing ones |
| De-obfuscate         | Medium               | A local model can add caveats and context, but can't add knowledge the answer lacks, and a small model rewriting facts can introduce errors | Annotate rather than rewrite: add notes and caveats around the answer, keep the provider's text intact and visible. Strip answers to decoy content using the request's record of what was added                           |
| Stitch split work    | Depends on the split | Pieces asked without context may not fit together; providers may contradict each other                                                      | Split only where parts are loosely coupled: general knowledge goes out, the local model applies it to the private details at home                                                                                         |

- **Whole-request federation needs no stitching.** Topic-pinned requests come back complete; only hybrid splits need reassembly.
- **Map out, reduce at home.** The cloud answers the general question ("how do these two drug classes interact?"); the local model applies it to the private case. This is the shape that stitches reliably.
- **Cross-checking is cheap stitching.** Sending the same clean question to two providers and comparing answers is easy, and catches errors.
- **Two transcripts.** Every conversation keeps a provider view (scrubbed) and a user view (restored). Follow-up turns send the provider view, never the restored text, so nothing leaks back out on turn two.
- **Streaming.** Surrogates can split across streamed chunks; buffer until each is complete before restoring.
- **Replies are untrusted input.** Restoration and correction run on text that may carry prompt injections or malicious links and code; they never trigger tools or outbound calls by themselves.

## Securing the gateway itself

The gateway is the only place the whole picture exists, so it must hold less than the providers hold today, or it just moves the leak indoors.

| Asset                 | Risk                                             | Control                                                                      |
| --------------------- | ------------------------------------------------ | ---------------------------------------------------------------------------- |
| Placeholder mappings  | A permanent decoder ring for everything scrubbed | In memory, one conversation, then deleted                                    |
| Logs                  | A full record of every request                   | Off by default; if on, short-lived, encrypted, scrubbed                      |
| Persona and topic map | A sketch of your life by topic                   | Local layer only, encrypted at rest                                          |
| API keys              | Spend and account takeover                       | Secret store (Bitwarden Secrets Manager or OS keychain), never a config file |
| Agent tool access     | Prompt injection from replies or web content     | Least privilege; no shell or file access unless a task needs it              |
| Cloud layer           | Sees scrubbed traffic, timing and volume         | No logs, nothing at rest, a host that is easy to move                        |

### Shutters

When something looks wrong, Bullbar closes, automatically and at the smallest level that contains the problem; reopening always needs a person.

| Level          | Example triggers                                                                                                      | What happens                                                                       |
| -------------- | --------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| 1 Provider     | Terms changed, endpoint misbehaving, exposure threshold reached, spend spike                                          | That provider leaves rotation; traffic reroutes within allowed rows                |
| 2 Component    | Scrubber crash or error, a component tries a network call it shouldn't, config or binary changed unexpectedly         | Component isolated; everything that depends on it stops                            |
| 3 Local only   | Outbound check finds a credential or PII the scrubber missed, cloud layer unreachable or suspect, home heartbeat lost | All egress stops; local models only, cloud requests queued or refused              |
| 4 Full shutter | Signs of compromise, repeated failed logins, lost device reported, manual panic                                       | Everything stops; placeholder maps in memory wiped, keys and client tokens revoked |

- **Fail closed.** Any failure in a protective part (scrubber, outbound check, vault) shuts the path, never bypasses it.
- **A second, independent outbound check.** A simple filter on the way out, separate from the scrubber, catches what the scrubber misses. Its hit is itself a trigger.
- **Heartbeats.** The cloud layer relays only while it hears from the local layer; silence closes it.
- **Manual panic button** on every client, including the phone.
- **Automatic to close, human to reopen.** Each shutter event is logged (what, when, why, without content) and needs review to lift.
- **Drill it.** Shutters are tested on a schedule, or they won't work on the day.

## Clients: desktop and mobile

Desktop and phone are both thin clients of an always-on internal server; neither runs the gateway itself.

- **Desktop (macOS or Linux):** any app that accepts a custom endpoint in the common chat-API format points at the gateway. Official consumer AI apps do not route through a proxy; this setup works with API access.
- **Mobile:** reaches the internal server over a private tunnel (WireGuard or Tailscale), never a port open to the internet. Few phone chat apps allow a custom endpoint, so the gateway serves a small web UI as the fallback.
- **Home offline:** the phone can reach the cloud layer, but the trusted layer is asleep. Either light pattern scrubbing runs on the phone, or sensitive requests queue until home is back. A refusal beats a leak.

## Hardware and the cloud mock

Tier 2 is the target: the design sends hard work out scrubbed, so the home box only needs to be smart enough to be the bouncer.

| Tier                     | Hardware (approximate)                          | Runs                                                                                             | Layers covered       |
| ------------------------ | ----------------------------------------------- | ------------------------------------------------------------------------------------------------ | -------------------- |
| 1 Gateway only           | Mini PC or old PC, CPU, 8–16 GB RAM             | Proxy, regex and PII scrubbing, routing rules, tunnel; sensitive work answered outside, scrubbed | 2 (partial), 4       |
| **2 Small local models** | 16–32 GB RAM; 8–12 GB VRAM GPU or Apple Silicon | Small models (about 3–8B, quantised) for triage, smart scrubbing, personas, chaff                | All four             |
| 3 Large local models     | 24–48 GB+ VRAM, or Mac with 64 GB+              | Large models doing sensitive complex work fully at home                                          | All four, less cloud |

### What the local runner adds

Three setups for the local runner slot: **empty**; **semi-trusted** (a model on hardware you don't control, which under rule 1 only ever sees scrubbed content, so it is an exposure row, not local; parked); and **trusted** (on the client itself, or on a server in your LAN or VPN). Most protection doesn't need the runner.

| Requests                                                 | Empty                       | Semi-trusted                       | Trusted                                                                  |
| -------------------------------------------------------- | --------------------------- | ---------------------------------- | ------------------------------------------------------------------------ |
| Always-on floor                                          | Yes                         | Yes                                | Yes                                                                      |
| Sensitivity rating                                       | Rules, or a CPU classifier  | No gain (would see raw content)    | Model                                                                    |
| Complexity rating                                        | Rules                       | On scrubbed text                   | Model                                                                    |
| Scrubbing: patterns (credential scanner)                 | Yes                         | Yes                                | Yes                                                                      |
| Scrubbing: entity recognition (CPU)                      | Yes, if the scrubber has it | Same                               | Same                                                                     |
| Scrubbing: context (relationships, indirect identifiers) | No                          | No                                 | Yes                                                                      |
| Secret; credentials with strip-and-send off              | Refused                     | Refused                            | At home                                                                  |
| Sensitive                                                | Leaves scrubbed             | Leaves scrubbed, less exposed      | Home, or scrubbed for hard work                                          |
| Splitting hard private work                              | No                          | No                                 | Yes                                                                      |
| Personas, decoy content                                  | No (nothing can undo them)  | No                                 | Yes                                                                      |
| Chaff                                                    | Off by default (canned)     | Weak (operator sees which is fake) | Yes                                                                      |
| Ledger, federation, terms register, "what leaves" view   | Yes                         | Yes                                | Yes                                                                      |
| Availability                                             | —                           | The operator's                     | Client: while it's on. LAN/VPN server: always, the phone over the tunnel |

| Replies                             | Empty                           | Semi-trusted                   | Trusted            |
| ----------------------------------- | ------------------------------- | ------------------------------ | ------------------ |
| Restore (mechanical)                | Yes; mangled surrogates flagged | Same (can't handle real facts) | Yes, plus repair   |
| Persona correction                  | — (no personas)                 | No (needs real facts)          | Yes, as annotation |
| Decoy stripping                     | Mechanical only, if declared    | Yes (clean content)            | Yes                |
| Chaff diversion (by tag, in core)   | Yes                             | Yes                            | Yes                |
| Chaff follow-ups                    | No                              | Weak                           | Yes                |
| Stitching: whole-request federation | Nothing to stitch               | Same                           | Same               |
| Stitching: split work               | No                              | No                             | Yes                |
| Cross-checking, answer check        | You judge                       | Yes (clean content)            | Yes                |

**Cloud mock first:**

- Build and test the plumbing on a rented server; rent GPU time by the hour to test models.
- **Synthetic data only.** The mock is a stage set: invented wives, imaginary patients, fake keys.
- Containers from day one, so moving home is copy and run.
- The model runner is just an address; the gateway shouldn't care where it lives.
- Measure the real workload on the mock, then buy hardware for real requirements. Check current prices at that point.

### Platform considerations

The gateway code is the same everywhere; what changes per platform is how models run, how it is packaged, and what the device will let it do in the background.

| Platform                       | Model runtime                                  | Packaging                                      | Watch out for                                                                       |
| ------------------------------ | ---------------------------------------------- | ---------------------------------------------- | ----------------------------------------------------------------------------------- |
| Tier 1 server (mini PC, Linux) | CPU only; small classifiers at most            | Containers                                     | Too slow for LLM triage; rules and PII tools only                                   |
| Tier 2/3 Linux + NVIDIA        | llama.cpp, Ollama or vLLM on CUDA              | Containers with GPU passthrough                | Driver and CUDA version pinning; power and noise if always on                       |
| Tier 2/3 Linux + AMD           | llama.cpp or Ollama on ROCm or Vulkan          | Containers, less smooth                        | Narrower support than NVIDIA; check the card first                                  |
| Tier 2/3 Mac (Apple Silicon)   | MLX, llama.cpp or Ollama on Metal              | Gateway in containers, **model runner native** | Containers on macOS can't use the GPU; unified memory makes large models affordable |
| Cloud layer (VPS)              | None, or rented GPU for the mock               | Containers                                     | Host jurisdiction and its legal access; datacenter IPs blocked by some services     |
| Desktop client                 | None                                           | Endpoint setting or browser                    | Only apps with a custom endpoint setting route through it                           |
| Android                        | Optional on-device model for offline scrubbing | Tunnel app + browser or custom-endpoint app    | Battery and background limits vary by maker                                         |
| iOS                            | Optional on-device model for offline scrubbing | Tunnel app + browser (web app)                 | Strict background limits; no system-wide proxy hooks for apps                       |

- **The model runner is just an address.** Keep it behind one interface so swapping Ollama for MLX, or home for cloud, doesn't touch the gateway.
- **Phones only need two things:** the tunnel and a browser. On-device models are a bonus for the offline fallback, not a requirement.
- **Pick the home box for the runtime you want,** not the other way round. Runtime and driver support changes fast; check current support before buying.

## Build phases

> **The order of work changed (2026-11-15).** The work so far went: the decision engine on its own; then Bullbar's own endpoint with the always-on floor and a cloud adapter behind it; then scrubbing and topic pinning. The classifier is next. Together these replace phases 1 and 2 below; the later phases still describe where the project is heading.

Build in order of payoff, so each phase is useful on its own if the project stops there.

1. **Gateway + scrubber (cloud mock).** Proxy with a standard chat-API endpoint, deterministic secret detection, PII placeholders, throwaway mappings. *Done when:* with strip-and-send enabled, a synthetic request with a fake key and a fake name goes out clean and comes back restored.
2. **Triage + routing.** Everything the local model can handle goes to it first and stays home; rules plus a small classifier send the rest to cheap or premium cloud. *Done when:* a test set of labelled prompts routes with no sensitive item sent to a public model.
3. **Mobile access.** Tunnel to the gateway, small web UI, offline fallback rule. *Done when:* the phone works away from home and refuses sensitive requests when the trusted layer is down.
4. **Federation.** Topic-pinned providers, separate accounts, cloud egress relay. *Done when:* each provider's request log contains only its assigned topics.
5. **Two-layer split.** Move the trusted layer onto home hardware; the cloud mock becomes the cloud layer. *Done when:* no mapping, persona or topic map exists outside the house.
6. **Obfuscation.** Personas per provider, decoy content, local chaff generation, local de-obfuscation. *Done when:* a month of traffic per provider reads as a consistent persona.

**Later, optional: Roo Spotter.** Automated side-channel monitoring and canary detection, built only once the core is stable and if manual canaries prove useful.

## Known limits and open questions

**Limits**

- No finder is complete: scrubbing lowers what leaves, it doesn't end it (see "No finder is complete" above). Relationships ("my wife") leak more than names for now.
- Scrubbing costs answer quality when the removed detail *is* the question.
- Obfuscation works against bulk statistical profiling, much less against a targeted investigation.
- Timing correlation: requests from one IP and one time window can be re-linked across providers unless egress and timing are varied.
- The cloud layer is a small aggregator of scrubbed traffic.
- Chaff costs money and may run against some providers' terms on automated traffic.
- The provider's API terms (no training, zero or short retention) remain the cheapest privacy win; choose providers on them.

**Open questions**

- [ ] Home hardware: Mac (which chip, how much RAM) or Linux (which GPU, how much VRAM)?
- [ ] Threat model: provider training, provider breach, employer, or government? Each changes the design.
- [ ] First content through it: health, personal documents, code, or email?
- [x] Build or assemble: write the glue only, or more? In what language? *Answered: assemble the plumbing (existing tools as sealed components), build the brain; Rust for the core and the hot path, other languages only where their ecosystem is unavoidable, each sealed in its own process.*
- [ ] API keys: held by the cloud layer, or signed per request by the local layer?
- [ ] Cloud host for the mock, and budget ceiling for it.

## Glossary

Two kinds of term. **Grouping terms** organise; they never name anything in code. **Functional terms** each name one thing in code, under the same name: a slot (`Slot::…`, a component with its own permissions and its own behaviour when empty), a mechanism (`Mechanism::…`, a step the engine engages per request) or a named part of core. *Planned* means named here, not yet in code.

**Grouping terms**

| Term            | Meaning                                                 | Members                                                     |
| --------------- | ------------------------------------------------------- | ----------------------------------------------------------- |
| Local layer     | Everything that knows who you are; it stays home        | Vault, local runner, scrubber, Deobfuscator, router         |
| Cloud layer     | What only ever handles clean content                    | Relay, provider adapters, providers                         |
| Always-on floor | What runs on every request, whatever its rating         | Credential scanner, outbound check, shutters, logging rules |
| Anonymisation   | Removing what links content to you (Layer 2, scrubbing) | Strip credentials, scrub, restore                           |
| Obfuscation     | Adding material that misleads (Layer 3)                 | Personas, decoy content, chaff                              |
| De-obfuscation  | Undoing obfuscation on the reply                        | Persona correction, decoy stripping, chaff diversion        |

**Functional terms: what leaves, and what comes back**

| Term               | In code                                                   | Meaning                                                                                                                                                       |
| ------------------ | --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Credential scanner | `Slot::CredentialScanner`                                 | Finds credentials (keys, tokens, passwords, card numbers) and strips them                                                                                     |
| Strip credentials  | `Mechanism::StripCredentials`                             | Replace credentials with placeholders before sending; done by the credential scanner                                                                          |
| PII scrubber       | `Slot::PiiScrubber`, `ScrubberGrade`                      | Finds and removes what identifies you, and restores it. Its grade (patterns, entities, context) is stated in `slots.toml`; patterns alone don't fill the slot |
| Scrub              | `Mechanism::Scrub`                                        | Remove personal details and relationships on the way out                                                                                                      |
| Restore            | `Mechanism::Restore`                                      | Put scrubbed details back into the reply; mechanical, with model repair of mangled surrogates sandboxed in the same slot                                      |
| Surrogate          | planned                                                   | A realistic stand-in for a scrubbed detail (a fake name of the same gender, a shifted date)                                                                   |
| Obfuscator         | `Slot::Obfuscator`                                        | Adds obfuscation on the way out; engages only what the Deobfuscator can undo                                                                                  |
| Persona            | `Mechanism::Personas`                                     | Misleading framing of a real query: a consistent invented identity per provider                                                                               |
| Decoy content      | `Mechanism::DecoyContent` (not engaged by any rating yet) | Plausible but irrelevant material inside a real query                                                                                                         |
| Chaff              | `Mechanism::Chaff`                                        | Additional plausible but irrelevant queries, sent as their own requests                                                                                       |
| Deobfuscator       | `Slot::Deobfuscator`                                      | Undoes obfuscation on the reply; no tools, no network, output to your view only                                                                               |
| Persona correction | `Capability::PersonaCorrection` (Deobfuscator)            | Annotate the reply for your real situation                                                                                                                    |
| Decoy stripping    | `Capability::DecoyStripping` (Deobfuscator)               | Remove the reply's answers to decoy content                                                                                                                   |
| Chaff diversion    | planned, in core                                          | Divert chaff replies by their home-only tag; fails closed                                                                                                     |
| Outbound check     | `Slot::OutboundCheck`                                     | A second, independent check on everything leaving; never depends on the scrubber                                                                              |

**Functional terms: routing**

| Term                  | In code                                                     | Meaning                                                                                                          |
| --------------------- | ----------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Sensitivity           | `Sensitivity`                                               | Public, Personal, Sensitive, Secret: sets the most exposed row                                                   |
| Complexity            | `Complexity`                                                | Easy, Medium, Hard: sets the minimum column                                                                      |
| Row                   | `Row`                                                       | Exposure: Local, AnonymousRelay, ProviderNoTraining, ProviderStandard                                            |
| Column                | `Column`                                                    | Capability: Small, Mid, Top                                                                                      |
| Cell                  | `Cell`                                                      | One row and column; the engine picks the cheapest allowed                                                        |
| Rung                  | `Attempt::rungs`                                            | One step up the exposure ladder, for a one-off exception                                                         |
| Credentials vs Secret | `RequestProfile::credentials_found` / `Sensitivity::Secret` | What the scanner finds vs a sensitivity rating                                                                   |
| Topic pinning         | `Mechanism::TopicPinning`                                   | Keep a topic on the provider it is pinned to (federation)                                                        |
| Exposure ledger       | `Slot::ExposureLedger`, `Mechanism::ExposureLedger`         | What each provider has seen, coarse and decaying                                                                 |
| Local runner          | `Slot::LocalRunner`                                         | Runs models on a device you control, reached without crossing the internet                                       |
| Capability            | `Capability`                                                | Something a slot does that others rely on, with the slot that provides it and what it needs (`Capability::spec`) |
| Effective capability  | `Capabilities`                                              | What the installed slots can actually do, given what each capability needs; `check-config` lists them            |
