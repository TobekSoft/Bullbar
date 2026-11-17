# Sample configs

Four editions of the same files. Pick one as a starting point. Account names are generic
(`provider-a`, `relay-free`); real providers belong only in adapter code.

| Edition | Who it's for | In short |
|---|---|---|
| `casual/` | Someone who wants convenience with sensible privacy | Defaults; strip-and-send on; escalates by itself |
| `paranoid/` | Someone who shares as little as possible | Tight ceilings; no lowering ratings; always asks; ledger on |
| `corporate/` | An admin setting rules for staff | Health, legal, finance and client work locked; no exceptions; no `user.toml` |
| `gateway-only/` | Hardware tier 1: a small box with no local model | Scrubbing and routing at home, every answer from outside; Secret refused |

## The files

- `providers.toml`: the accounts. `row` is who sees a request (`local`, `anonymous-relay`,
  `provider-no-training`, `provider-standard`). `offers` maps each column (`small`, `mid`, `top`)
  to a relative cost; only the order matters, and local is free. `review_due` is when the
  account's terms are next due for review: after it, the account counts one row more exposed.
  `status` is `active` (the default), `demoted` (the same, until reviewed) or `out`.
  `base_url` and `models` (together or not at all) say where `bullbar serve` reaches the account
  and which model it asks for in each column it offers; without them the account is for dry runs
  only. A `local` account's address must be loopback or on a private network, and any other
  account's must be https (plain http only to loopback), or `serve` won't start. `key_file` names
  the file holding the account's API key, relative to the config folder; the key is read when
  `serve` starts and never printed. A key file that can't be read leaves that account out, and
  `check-config` and `serve` both say so. Keep key files in `keys/` (ignored by git) and readable
  only by you (`chmod 600`).
- `policy.toml`: the admin's level (ADR 0012). Anything left out takes the default shown in the
  casual edition, which spells them all out. `default_complexity` is the complexity of a request
  whose model name doesn't give one (`bullbar` rather than `bullbar/hard`).
- `user.toml` (optional): the user's level. Only tightens the admin's; a config that loosens past it
  is rejected on load. Anything left out takes the admin's value. One setting has no admin value:
  `chaff_without_local` (default `false`) sends canned chaff when there is no local runner to write
  it (ADR 0015).
- `slots.toml`: which components are installed. Anything not listed is empty, and Bullbar won't start
  without the five required ones. With `pii-scrubber` installed, `pii_scrubber_grade` must say what it
  can find: `patterns` (counts as no scrubber), `entities` (names, places, dates; the least that fills
  the slot) or `context` (relationships and indirect identifiers too, using the local runner).
- `ledger.toml` (optional): the exposure ledger stub, counts of what each account has seen.

Unknown keys are errors, so a typo fails loudly instead of doing nothing.
