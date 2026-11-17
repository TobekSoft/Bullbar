# Running Bullbar on Linux, with Jan as the chat client

Step by step: install what Bullbar needs, set it up, run it, and chat through it from Jan. Then the
settings you can change, and how to see exactly what leaves (`trace`).

Written for Linux Mint 22 (Ubuntu 24.04 based); other Debian-family distributions should work the
same. Everything runs in containers, so you don't need Rust unless you want to build Bullbar's
command line on the host (step 8, optional).

What you get: Bullbar on `127.0.0.1:8480`, a local model runner it alone can reach, and, if you add
one, a cloud account it sends work to once the work is scrubbed.

## 1. Install the prerequisites

```bash
sudo apt install podman docker-compose-v2 git curl
systemctl --user enable --now podman.socket
```

- **Podman** runs the containers, rootless (as you, not as root).
- **A compose provider:** `podman compose` hands the compose file to an external tool. Bullbar was
  tested with Docker Compose's plugin (`/usr/libexec/docker/cli-plugins/docker-compose`, there from
  Docker's own `docker-compose-plugin` package; `docker-compose-v2` is the distribution's package of
  the same plugin). `podman-compose` (`sudo apt install podman-compose`) should also do, untested.
  Docker itself isn't needed.
- **The Podman API socket** is what Docker Compose talks to.
- **curl** fetches the scrubber's model (step 4).

Check: `podman compose version` names the provider it found.

## 2. Get Bullbar

```bash
git clone <repository address> bullbar
cd bullbar
```

Every command below runs from this folder.

## 3. Start Bullbar and the local runner

```bash
podman compose up -d
```

This builds Bullbar's image (a few minutes the first time) and starts two containers: Bullbar and
Ollama, the local model runner. Then give the runner a model:

```bash
podman compose run --rm models pull <model>             # a small model of your choice
podman compose run --rm models cp <model> local-small   # the names the sample config asks for;
podman compose run --rm models cp <model> local-mid     # one model can fill both
```

Check:

```bash
podman ps                       # bullbar-bullbar-1 and bullbar-ollama-1, both "Up"
podman logs bullbar-bullbar-1   # what this setup does, and what it does less
```

What it sets up:

- **The local runner has no way out.** It sits on a network only Bullbar can reach. Models are
  pulled by the separate `models` service, which runs only when you ask.
- **Both containers run as non-root**, with read-only filesystems and no capabilities. Ports are
  open on this machine's loopback only.
- **Bullbar's state** (tokens, the shutter, pins, trace) is on the host, in
  `~/.local/state/bullbar/`.

## 4. Turn on scrubbing (recommended)

Without it, Personal work stays on the local runner. With it, Personal work can leave, scrubbed:
names swapped for fake ones, addresses and phone numbers for tokens, all put back in the answer.

```bash
scripts/fetch-pii-model.sh                                  # once: about 1.2 GB into models/
podman compose -f compose.yaml -f compose.scrub.yaml up -d  # Bullbar again, with the model
```

The script checks each file against pinned hashes. If a file doesn't match, the script deletes it
and stops.

From here on, **every `up` needs both `-f` files**. If you leave them out, Bullbar starts without
scrubbing (and says so in its log).

Try the scrubber on its own. Nothing is sent anywhere:

```bash
echo "My neighbour Imogen Fakeworth lives at 12 Fakery Lane, phone 07700 900123." | podman exec -i bullbar-bullbar-1 bullbar scrub --model /models/pii
```

## 5. Add a cloud account (optional)

Without one, everything is answered locally, or Bullbar says why it can't answer. The sample
accounts are placeholders (their addresses never resolve). To use a real account:

```bash
mkdir -p container/local
cp container/providers.toml container/local/providers.toml   # ignored by git
```

1. In `container/local/providers.toml`, edit `[accounts.provider-a]`: set `base_url` to the
   provider's OpenAI-style API address (ending in `/v1`, or whatever the provider documents) and
   `models` to its model names, one each for `small`, `mid` and `top`. Its `row` says how exposed it
   is: `provider-no-training` if the account's terms promise no training on your data, otherwise
   `provider-standard`. Remove the accounts you don't have.
2. Put the account's API key in `config/casual/keys/provider-a.key` (key files are ignored by git),
   readable only by you: `chmod 600 config/casual/keys/provider-a.key`.
3. Start with your accounts:

   ```bash
   BULLBAR_PROVIDERS=container/local/providers.toml podman compose -f compose.yaml -f compose.scrub.yaml up -d
   ```

   `BULLBAR_PROVIDERS` takes a path inside this folder, without a leading `./`. As with the `-f`
   files, include it on every `up`, or Bullbar goes back to the placeholder accounts.

## 6. Install Jan and point it at Bullbar

> **Not yet checked against Jan's screens.** The settings below are what Jan needs. The menu names
> are inferred and may differ in your version of Jan.

1. Install Jan from its website (Linux: a `.deb` package or an AppImage).
2. In Jan's settings, under model providers, add an **OpenAI-compatible** provider:
   - **Base URL:** `http://127.0.0.1:8480/v1`
   - **API key:** the contents of `~/.local/state/bullbar/chat.token`. Open the file in a text
     editor and copy the key from there. Bullbar never prints it, and only you can read the file.
   - **Models:** Jan may fetch the list from Bullbar. If not, add the ones you want by hand
     (next step). The list has each rating and each rating with a complexity, never a category:
     a name with a category (`bullbar/personal/hard/health`) works, but you add it by hand.
3. Start a new chat and pick a model.

**The model name tells Bullbar about the request**, since Jan, like most chat clients, lets you pick a
model but not add anything else:

```
bullbar/<rating>/<complexity>/<category>     any order, any part left out
```

| Part       | Values                                                          | Left out           |
|------------|-----------------------------------------------------------------|--------------------|
| rating     | `public`, `personal`, `sensitive`, `secret`                     | treated as sensitive |
| complexity | `easy`, `medium`, `hard`                                        | `medium` (policy)  |
| category   | `general`, `health`, `legal`, `finance`, `work-code`, `personal` | `general`          |

For example `bullbar/public/easy` (anything, cheapest model), `bullbar/personal/hard` (about you,
scrubbed, a capable model), `bullbar/personal/hard/health` (also held to the health category's
limit). Secret never leaves this machine.

Each answer ends with a footer saying where it came from ("answered by provider-a at
provider-no-training / top"), and any notes on what was scrubbed or stripped.

Things Jan does that you may notice:

- **Thread titles:** Jan asks the model for a title, sending the whole conversation as one message.
  That request goes through Bullbar too and is scrubbed like any other.
- **Old chats carry their history.** After changing Bullbar's setup, start a new chat to see the
  change cleanly.

## 7. When Bullbar asks you something

Some decisions are yours: sending a request one step more exposed than its rating allows, or moving
a topic to another account. Bullbar shows the question in the chat and waits (up to two minutes on a
streamed reply, which is how Jan asks). Answer from a terminal:

```bash
podman exec bullbar-bullbar-1 bullbar pending             # open questions, with their ids
podman exec bullbar-bullbar-1 bullbar answer <id> yes     # or no; no answer counts as no
```

If the wait runs out, the chat says where to answer. Answer, then send the message again.

## 8. Everyday commands

```bash
podman exec bullbar-bullbar-1 bullbar status    # the shutter, open questions, counts
podman exec bullbar-bullbar-1 bullbar kill      # close the shutter: nothing leaves; local answers go on
podman exec bullbar-bullbar-1 bullbar reopen    # open it again (kept across restarts until you do)
podman logs bullbar-bullbar-1                   # start-up notices; never request content
podman compose stop                             # stop both containers
```

Optional: with Rust installed (`rustup`), `cargo build --release` builds the same command line on
the host as `target/release/bullbar`. It works against the running containers without
`podman exec`, since the state folder is shared: `target/release/bullbar status`.

## 9. Seeing what leaves: trace

Nothing about requests is written anywhere by default. When you want to see exactly what a provider
received, turn on a trace for a number of requests:

```bash
podman exec bullbar-bullbar-1 bullbar trace 5      # the next 5 requests that leave (1 to 100)
# chat as usual, then:
cat ~/.local/state/bullbar/trace.jsonl
podman exec bullbar-bullbar-1 bullbar trace off    # stop early; the file stays
podman exec bullbar-bullbar-1 bullbar trace clear  # stop, and delete the file
```

- Each line of `trace.jsonl` is one request as it was sent (scrubbed) and the provider's raw reply
  or error. That is what the provider already has: surrogates and placeholders, never your
  originals. Requests answered locally aren't traced.
- The trace stops by itself after that many requests, or after an hour, whichever comes first.
- **Jan's title requests count** toward the number, so allow a few extra.
- Delete the file when you're done (`trace clear`). It holds what you sent, even if scrubbed.
- An administrator can forbid tracing (`trace_allowed = false` in `policy.toml`).

## 10. Settings

### Your choices: `config/casual/user.toml`

Bullbar reads its config at start, so restart it after a change (step 3's or 5's `up -d`, or
`podman compose restart bullbar`). It refuses to start if a value goes beyond what the policy
allows, and says which.

| Setting                        | Values                                                    | Casual sample |
|--------------------------------|-----------------------------------------------------------|---------------|
| `strip_and_send`               | `true`: credentials are replaced so the rest can leave; `false`: a request with one stays local | `true` |
| `escalation`                   | when an answer is judged poor: `auto`, `ask-before-leaving-local`, `ask`, `off` | `auto` |
| `exceptions`                   | offer one-off steps one rung more exposed, confirmed each time | on (policy) |
| `max_column`                   | most capable model column used: `small`, `mid`, `top`     | `top` |
| `send_sensitive_without_local` | with no local model, Sensitive leaves scrubbed instead of being refused | on (policy) |
| `chaff_without_local`          | send canned decoy traffic without a local runner (costs money, easy to filter) | `false` |
| `[ceilings]`                   | most exposed row per rating: `public = "…"`, `personal = "…"`, `sensitive = "…"` | the policy's |
| `[categories]`                 | your own per-category limits, e.g. `health = "local"`    | none |

Rows, from least to most exposed: `local`, `anonymous-relay`, `provider-no-training`,
`provider-standard`. You can tighten a ceiling as far as you like. You can loosen one only up to
the policy's limit.

### The limits: `config/casual/policy.toml`

The administrator's file (on your own machine, that's you too). It sets each rating's most exposed
row, which complexity needs which model column, per-category limits (`locked = true` stops users
loosening them), and what users may switch on: `strip_and_send_allowed`, `exceptions_allowed`,
`lower_ratings_allowed`, `send_sensitive_without_local_allowed`, `trace_allowed`. The sample's
comments say what each does.

Editing the sample files shows up as changes in git. That's harmless, but keep a copy if you
update the repository.

### Bullbar's own options

The ones that matter day to day, as `bullbar serve` takes them:

| Option              | What it does                                                      | Default |
|---------------------|-------------------------------------------------------------------|---------|
| `--no-footer`       | leave out the line saying where each answer came from              | footer on |
| `--log decisions`   | log each decision (outcome, route, reasons; never content) to the container log | off |
| `--hold <s>`        | how long a non-streamed request waits for your answer to a question | 20 |
| `--stream-hold <s>` | the same for a streamed reply (Jan's)                              | 120 |

In the containers they go in the start command. Create an override file of your own in
`container/local/` (ignored by git), for example `container/local/options.yaml`, holding:

```yaml
services:
  bullbar:
    command: [serve, --config, /config, --listen, "0.0.0.0:8480", --control, "0.0.0.0:8481",
              --pii-model, /models/pii, --log, decisions]
```

Keep the first six arguments as they are. Leave out `--pii-model /models/pii` if you don't use
scrubbing. Then add the file last on every `up`:

```bash
BULLBAR_PROVIDERS=container/local/providers.toml podman compose -f compose.yaml -f compose.scrub.yaml -f container/local/options.yaml up -d
```

Then, to follow the log (with `--log decisions`, one line per decision):

```bash
podman logs -f bullbar-bullbar-1
```

## When something goes wrong

- **Jan can't connect:** `podman ps` (both containers up?), and that the base URL ends in `/v1`.
- **401 or "unauthorised":** the API key in Jan isn't the current `chat.token`.
- **An answer says Bullbar answered itself:** the footer or reply says why (a protection not set up,
  the shutter closed, no account for that row). `podman logs bullbar-bullbar-1` shows what this
  setup does less.
- **A provider error:** the reply says what kind of failure it was and which fields the request
  carried, never what the provider wrote. Turn on a trace (step 9) and send the message again to
  see the provider's own reply.
- **After pulling a new version:** `podman compose ... up -d --build` with your usual `-f` files.
