# Feature: OS credential store for install secrets

> Last updated: 2026-11-27

## Context

The install secrets (`JWT_SECRET`, `CRYPTO_ENCRYPTION_KEY`) were plaintext
files under `PICSOU_SECRETS_DIR`. On desktop machines that means the database
encryption key sits on the disk next to the database. This feature keeps those
two secrets in the OS vault instead, where the platform seals them (DPAPI on
Windows, Secret Service on Linux).

## How it works

`PICSOU_OS_KEYSTORE` selects the behavior: `off` (files, as before), `auto`
(store first, files as migration source and fallback), `require` (store only,
fail closed). The desktop shell starts the backend in `auto`; an explicit user
`require` is honored, anything weaker still starts in `auto`.

On first boot with the store enabled, each secrets file is moved into the vault
entry `picsou-finance` / `<jwt_secret|crypto_key>`, read back to prove the
save, then deleted. From then on the files are gone and the store is the only
copy. A stale file next to a populated store is ignored with a warning, never
read. Explicit `JWT_SECRET` / `CRYPTO_ENCRYPTION_KEY` environment variables
keep winning over both files and store.

### Key files

- `backend-rs/src/crypto/keystore.rs` — store mapping (`SecretStore` trait,
  `KeyringStore`, modes). The trait keeps the tests off the real vault.
- `backend-rs/src/crypto/secrets.rs` — `bootstrap_with`, migration, fallback,
  fail-closed paths.
- `desktop/src-tauri/src/supervisor.rs` — `backend_env` sets `auto`,
  `merge_backend_env` only ever strengthens to `require`.

### Flow

```
backend start → secrets::bootstrap
  → mode off      → files (unchanged behavior)
  → mode auto     → store hit? return : file? migrate : generate into store
                    store error? warn + files
  → mode require  → store hit? return : file? migrate : generate into store
                    store error? refuse to start
```

## Technical choices

| Choice | Why | Rejected alternative |
|--------|-----|----------------------|
| `keyring` crate, no raw TPM code | Credential Manager / Secret Service already seal with the TPM where one exists; no ceremony, PIN, or PCR policy to operate | `tss-esapi` sealed blobs: operational cost, no recovery story |
| `auto` default on desktop only | Migrates existing installs with no flag day; servers keep deterministic file behavior | `require` everywhere: bricks headless installs without a vault daemon |
| Verify-after-save, fail closed | A store that accepts but loses a write must never leave two competing keys | Trust the save return code alone |
| User env can only strengthen | The shell must not let a stray `PICSOU_OS_KEYSTORE=off` silently downgrade the install | Pass the variable through untouched |

## Gotchas / Pitfalls

- The vault is per-user, per-machine: a `picsou.db` backup restored on
  another machine needs its vault entries too, or the encrypted columns stay
  unreadable. Back up both.
- `POSTGRES_PASSWORD` never enters the store (operator-supplied, never
  generated).
- The setup wizard still writes a key file at `APP_CRYPTO_KEY_PATH` when it
  generates one; the next boot migrates it into the store and deletes it.
- On Linux without Secret Service or keyutils, `auto` warns and keeps files;
  only `require` guarantees the seal (by refusing to start without it).

## Tests

- `crypto::secrets::tests` — file behavior unchanged in `off`; migration,
  generation, fallback, fail-closed, and lost-write paths with in-memory and
  failing fake stores.
- `crypto::keystore::tests` — mode parsing.
- `supervisor::tests` — shell defaults to `auto`, honors only `require`.

## Links

- Related ADR: [2026-11-27-os-keystore](../decisions/2026-11-27-os-keystore.md)
- Follows: [2026-04-08-mandatory-encryption-key](../decisions/2026-04-08-mandatory-encryption-key.md)
- Next step: Windows Hello / Linux prompt as the interactive unlock gate, on
  top of this store.
