# How Bullbar works

What goes on under the hood when a chat client sends Bullbar a message: which part acts on it, in
what order, what can stop it, and how the answer comes back. The structure (the crates, the config
files, the commands) is in the README and `docs/running-with-jan.md`;
the intent behind it all is in the [concept design](concept-design.md).

**As built on 2026-11-17** (release prep, after chunk 3). Only what exists is described. Parts the
design has but the code doesn't yet are named by what Bullbar does without them, which is what it
does today. Each step names where it lives in the code, so a chunk that changes the flow knows what
to update here.

## The shape, in pattern names

For readers who know the usual names. Each is one line; the steps below are the detail.

| Pattern | In Bullbar |
|---------|-----------|
| **API gateway** (one front door that every request comes through) | `bullbar serve`: an OpenAI-style chat endpoint; clients talk only to it |
| **Policy decision point / policy enforcement point** (one part decides, another carries it out) | the decision engine in `bullbar-core` decides, pure logic with no I/O; the gateway enforces, and checks again before anything leaves |
| **Content-based router** (the route depends on what the message is) | the engine picks an account by the request's ratings (sensitivity, complexity, category), never by reading its text |
| **Pipes and filters** (a fixed line of steps, each doing one thing) | on the way out: strip credentials, scrub, outbound check; on the way back: keep known fields, restore, footer |
| **State machine** | the request lifecycle: decided, waiting for the user, sent, refused |
| **Ports and adapters** (fixed sockets, swappable parts) | slots: each slot is a contract; components fill them; an empty slot has a documented safe behaviour |
| **Adapter** (translates to one outside system) | the cloud adapter: one OpenAI-style client per account, with a key of its own and nothing taken from the environment |
| **Pseudonymisation** (real details swapped for consistent fakes, reversible only at home) | the PII scrubber: fake names from a keyed hash, tokens such as `[ADDRESS_1]`; the map never leaves |
| **Control plane / data plane** (management kept apart from traffic) | the control channel: its own port and its own token; chat clients can ask, only the user answers |
| **Fail closed** | any protective part that fails blocks the request (design rule 4) |

```
 chat client ──► chat endpoint ──► decision engine ──► floor ─────────────► provider
   (Jan)         token, model        where may it go?   strip credentials     (cloud account)
                 name, facts                            scrub details      or local runner
                                                        outbound check
 chat client ◄── reply: keep the answer's fields ◄── restore details ◄── answer
                 footer (where it came from, what was replaced)

 terminal ──► control channel: pending / answer, kill / reopen, status, trace
```

## 1. Starting up

`bullbar serve --config <dir>` (`crates/bullbar-cli/src/main.rs`, then `server::serve` and
`server::router` in `crates/bullbar-gateway/src/server.rs`).

1. **Config is read and checked** (`bullbar-core/src/config.rs`). Five files: `providers.toml`
   (accounts), `policy.toml` (the admin's limits), `user.toml` (the user's choices inside them),
   `slots.toml` (which slots are filled) and the ledger stub. A user value looser than the policy
   allows stops the start, naming the file and key.
2. **It says what it is:** the "serving" line, then the proof-of-concept notice.
3. **The entity model loads** if `--pii-model` names it (about 1.2 GB; a model that won't load stops
   the start rather than running without it).
4. **Each account is wired up** (`router`):
   - a local account's address must be on this machine or the local network (`address.rs`), and
     it gets no proxy, so local content can't leave through one;
   - a cloud account's key is read from its `key_file` (`key.rs`); a key that can't be read leaves
     that account out, with a notice ("account `provider-b` left out");
   - each account gets its own HTTP client with fixed timeouts, and nothing from the environment
     (a key meant for one provider can't reach another).
5. **The engine is built** (`bullbar-core/src/engine.rs`, `Engine::new`). The five required slots
   must be filled (decision engine, outbound check, shutters, policy, credential scanner), or
   Bullbar refuses to start. Accounts the slots can't serve are dropped: local ones without a local
   runner, cloud ones without provider adapters; without a terms register, every cloud account
   counts as standard terms.
6. **The scrubber's grade is compared with the config's** (`scrubber_grade`): configured for
   `context` but built for `entities` gives the start notice you see in the log; with no model at
   all, the scrubber counts as empty and Personal work stays local. Then each slot `slots.toml`
   declares but this version doesn't build gets a line saying what stands in for it (`not_built`;
   section 5).
7. **State is loaded from the state folder** (`~/.local/state/bullbar`): the chat and control
   tokens (made on first start, readable only by the user, never printed; `token.rs`), the
   scrubbing secret (`scrub.secret`, for fake names), the topic pins (`pins.json`) and the shutter
   (a closed shutter stays closed across restarts; a file that can't be read counts as closed).
8. **Two listeners open:** the chat endpoint (default `127.0.0.1:8480`) and the control channel
   (`127.0.0.1:8481`). The trace is off at every start.

## 2. A request on its way out

`server::chat`, then `drive`, `carry_out`, and `forward` or `open_stream`.

### At the door

1. **The chat token.** No token, or the wrong one: `401`, nothing else happens
   (`require_chat_token`).
2. **The model name becomes a profile** (`model_name.rs`). `bullbar/personal/hard/health` gives a
   rating, a complexity and a category; parts left out take defaults (no rating counts as
   Sensitive, complexity from the policy, category `general`). The engine only ever sees this
   profile, never the text.
3. **Bullbar's own words are removed** (`reply::strip_own`). Clients send the whole conversation
   back each turn, footers and Bullbar's questions included; those are dropped first, so a model
   never sees Bullbar's side, even inside a transcript a client copied into one message.
4. **Facts are gathered for the profile:** whether the credential scanner finds a credential
   anywhere in the request (`floor/scanner.rs`), whether the shutter is closed, and which account
   this category is pinned to, if any (`pins.rs`).
5. **A question already answered?** If this conversation's question was answered on the control
   channel, the request picks up from that answer, but only if a fresh decision would still ask
   the same question (`pending.rs`).

### The decision (the engine, `engine.rs`)

Pure logic: no network, no files, and every step adds a reason to the explanation, so no decision
is silent. In order:

1. **The rating:** the user's (from the model name) or the classifier's. The classifier isn't built
   yet, so it is always the user's, and an unrated request counts as Sensitive.
2. **Can it leave at all?** Secret never leaves. Credentials found keep it local unless
   strip-and-send is on, in which case they will be replaced. This is enforced by a type:
   a cloud route can't be built without a proof the request may leave (`eligibility.rs`).
3. **The ceiling:** the most exposed row the rating may reach (`policy.toml`, `user.toml`), lowered
   by a stricter category limit, a one-off exception asked for after a poor answer, a
   closed shutter (local only), or no scrubber for Personal and Sensitive work (local only).
4. **The minimum column:** the least capable model the complexity needs, capped by the user's
   `max_column`.
5. **The accounts in play:** those not marked out, not overdue for review (an overdue account is
   demoted, never promoted), and inside the ceiling.
6. **Pinning:** Personal and Sensitive work goes only to its topic's pinned account. If
   the pinned account can't take it but another could, Bullbar asks before moving it; it never moves
   quietly.
7. **The cheapest cell inside both** (row at or under the ceiling, column at or over the minimum).
   None? Bullbar offers to settle for the best local model, and asks; with no local model, it
   refuses.
8. **The protections the route needs**, by rating: Personal and up get scrub, restore and topic
   pinning; Sensitive also wants personas, chaff and the exposure ledger; credentials found add strip
   credentials. A protection whose slot is empty is named in the explanation.

The outcome is one of three: **a route** (account, row, column, protections), **a question for the
user**, or **a refusal**. The lifecycle (`lifecycle.rs`) holds it.

Try any of this without sending anything: `bullbar decide --config config/casual --complexity hard
--sensitivity personal` prints the route and the numbered reasons.

### A question for the user

The request is parked (`pending.rs`), in memory, keyed by a hash of the conversation (never the
conversation itself). A plain request is held open for up to 20 seconds; a streamed one shows the
question at once and stays open for up to 2 minutes. The user answers with `bullbar answer <id>
yes|no` on the control channel. Answered in time, the request goes on; otherwise the chat says where
to answer and to send the message again. No answer counts as no, and a restart forgets every
question, so nothing is ever loosened by default.

### Carrying out a route (`carry_out`)

1. **A local route goes straight to the local runner**, as written: the runner is ours, sits on a
   network with no way out, and gets the request unchanged.
2. **A route that leaves is checked again, at the moment of leaving:**
   - the shutter (a question answered after the shutter closed carries a decision made before it);
   - every protection the route lists must be one the gateway carries out (today: strip
     credentials, scrub, restore, topic pinning). Personas, chaff and the exposure ledger aren't
     built, so a route that lists them is answered by Bullbar with "Bullbar can't do that yet" and
     nothing is sent;
   - the account must have an address.
3. **The floor** (`floor/mod.rs`, `prepare_with`), off the async threads since scrubbing runs a
   model (about 0.06 s a message):
   1. **Strip credentials** if the route says so: each becomes `[CREDENTIAL_1]`, `[CREDENTIAL_2]`...
      If credentials are found and the route doesn't strip them, the request is blocked. Credentials
      go first, so they are never mistaken for details that get restored.
   2. **Scrub** if the route says so (`scrub/conversation.rs`). The whole conversation, every
      message, each turn: details are found by patterns (`scrub/patterns.rs`: phones, emails,
      addresses, dates, identifiers) and by the entity model (`scrub/entities.rs`: names, places,
      organisations), then each distinct value gets one replacement wherever it appears. People get
      fake names, made from the value, the conversation's first message and the install's secret
      (`scrub/surrogate.rs`), so the next turn gets the same ones without anything being stored;
      relationships ("my wife") and other details get tokens (`[RELATIONSHIP_1]`, `[ADDRESS_1]`).
      A scrubbing failure blocks the request.
   3. **The outbound check** (`floor/outbound.rs`): the last look at the exact text about to leave.
      Its own rules, written separately from the scanner's, plus a search for every credential
      stripped and every detail replaced, raw or encoded. A hit blocks the request and says what kind
      it found and where, never the value. It only ever stops a request; it never changes one.
4. **The pin is recorded** before anything leaves: the first account a topic goes to becomes its
   pin.
5. **The restorer is built** from the replacement map. If it can't be, nothing is sent: an answer
   that couldn't be restored isn't asked for.

### Leaving (`forward`, `open_stream`)

1. **Addressed:** the account's own model name for the chosen column replaces `bullbar/...`, and for
   a cloud account only the chat API's own fields go (`fields.rs`); anything else stays home and is
   named in the footer ("not sent (not in the chat API): ...").
2. **Traced**, if a trace is on (`trace.rs`): the body exactly as sent, already scrubbed.
3. **Sent.** A failure says what kind it was and which fields the request carried, never what the
   provider wrote (its message can quote the request), and nothing is sent anywhere else.

## 3. The answer coming back

Plain (`restore_answer`) or streamed (`stream.rs`, `Writer::relay`), the same steps:

1. **Traced first,** if a trace is on: the provider's raw reply, before anything is changed.
2. **Only the answer's own fields** go on (`fields::answer_only`): role, content, tool calls,
   refusal. The model's reasoning and any extras stay behind, as on the way out.
3. **Details are restored** (`scrub/restore.rs`): each fake name or token is put back to what it
   stood for, whole words only, in the case it came back in. In a stream, text that might be the
   start of a token is held back until it's complete. What can't be matched exactly is never
   guessed: a detail that didn't come back, a token that came back changed, and a token Bullbar never
   sent are named in the footer. Credential placeholders are never restored.
4. **The footer** goes after the answer (`server::footer`): where it came from ("answered by
   provider-a at provider-no-training / top"), fields not sent, what each credential placeholder
   stands for, what didn't restore exactly, and a line when a trace logged it. `--no-footer` drops
   the first two; the rest are always said, since they are things the user must not be surprised by.

A local answer takes the same path, with nothing to restore.

## 4. Alongside: the control channel

`control.rs`, its own listener and token. Chat clients can never use it: a provider's
reply can print anything, so an answer typed into the chat couldn't be trusted to come from the user.

- `pending`, `answer <id> yes|no`: Bullbar's questions.
- `kill`, `reopen`: the shutter (`shutter.rs`). Closed, nothing leaves; local answers go on.
- `status`: the shutter, open questions, and counts since start (`activity.rs`), never content.
- `trace <n>`, `off`, `clear`: what leaves, for the next n requests or an hour.

Logs hold no content: start notices, and with `--log decisions` each decision's outcome, route and
reasons.

## 5. Slots: what's there, and what Bullbar does without the rest

| Slot | Today | Without it |
|------|-------|-----------|
| Decision engine, policy, shutters (kill switch only), credential scanner, outbound check | built; required | Bullbar refuses to start |
| PII scrubber | built: patterns plus the entity model (grade `entities`) | patterns alone count as none: Personal and Sensitive work stays local |
| Local runner | Ollama, on a network with no way out | Sensitive can leave scrubbed or be refused (`send_sensitive_without_local`); local accounts are dropped |
| Provider adapters | one OpenAI-style adapter | cloud accounts are dropped |
| Federation | topic pinning per category | no pinning |
| Classifier | **not built** | the rating comes from the model name; unrated counts as Sensitive |
| Answer check | **not built** | answers are never judged, so nothing escalates by itself |
| Terms register | **not built**; each account's row is set by hand in `providers.toml` | every cloud account counts as standard terms |
| Exposure ledger | a fixed stub | not counted |
| Obfuscator, deobfuscator, canaries | not built | personas and chaff are named as unavailable; decoys aren't asked for |
| UI | none; the chat client and the command line | |

**Declared versus built.** `slots.toml` says which slots are filled, and the engine believes it.
The sample configs declare some slots that aren't built (casual: the classifier, answer check and
terms register; the others also the exposure ledger, obfuscator, deobfuscator and canaries). `serve`
names each at start, with what stands in for it (`NOT_BUILT` in `server.rs`):

```
bullbar: the classifier is declared in slots.toml but not built: ratings come from the model name; unrated work counts as Sensitive
bullbar: the terms register is declared in slots.toml but not built: each account's row is as set by hand in the accounts file
bullbar: the answer check is declared in slots.toml but not built: answers aren't judged, so nothing escalates by itself
```

They aren't treated as empty, as a weaker scrubber is: an empty terms register would count every
cloud account as standard terms, and the rows in `providers.toml` are your own judgement.

## 6. What the dry run doesn't know

`bullbar decide` and `bullbar scenarios` run the engine on config alone. They don't read key files
or check addresses, so they can name an account `serve` would leave out: the casual sample routes
public hard work with a credential to `provider-b` (cheaper), while a `serve` with no key file for
`provider-b` sends it to `provider-a`. The start notices say which accounts were left out.

## Where to go from here

- What the design is heading for (obfuscation, federation, the ledger): the
  [concept design](concept-design.md).
- Running it: [running-with-jan.md](running-with-jan.md).
- Seeing it: `bullbar decide` (the engine), `bullbar scrub` (the scrubber), `bullbar trace` (what
  left), `--log decisions` (every decision's reasons).
