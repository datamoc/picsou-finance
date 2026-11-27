# ADR: Install secrets in the OS credential store

> Date: 2026-11-27
> Status: 🟡 Proposed

## Context

`JWT_SECRET` and `CRYPTO_ENCRYPTION_KEY` live as plaintext files under
`PICSOU_SECRETS_DIR` (`0600`). Anyone who can read the data folder — a stolen
laptop disk, a backup that travels off-host — gets the database encryption key
directly. The 2026-04-08 ADR made the key mandatory; this one removes it from
the disk on the desktop installs, where a platform vault exists.

## Decision

1. **A `crypto::keystore` module maps the two generated secrets onto the OS
   credential store** (`keyring` crate, service `picsou-finance`): Credential
   Manager on Windows (DPAPI user scope, whose master keys are TPM-backed on
   machines with a TPM), Keychain on macOS, Secret Service / kernel keyring on
   Linux. No raw TPM code: the platform stores own the sealing.
2. **`PICSOU_OS_KEYSTORE` selects the behavior**: `off` (today's files),
   `auto` (store first, files as migration source and fallback),
   `require` (store only, fail closed). Unknown values refuse to start.
3. **Migration is automatic and one-way**: first boot with `auto`/`require`
   moves a secrets file into the store, reads it back, then deletes the file.
   A save the store does not read back is always a startup error, never a
   second competing value.
4. **The desktop shell starts in `auto`** and only honors an explicit user
   `require`: the user's environment can strengthen the choice, never weaken
   it back to plaintext. Docker and Helm installs keep the files (`off`
   default), where no vault can be assumed.
5. **Explicit environment variables still win** and never touch the store:
   an operator-supplied secret keeps today's file behavior (`auto`) or skips
   files entirely (`require`, with a warning when a stale file remains).

## Alternatives considered

### Raw TPM2 sealing (tss-esapi, sealed blobs with PCR policy)

- **Pros**: the key never leaves the TPM; boot-state binding.
- **Cons**: a new sealing ceremony per platform, PIN/auth-value management, no
  recovery story, PCR brittleness across updates. The OS stores already seal
  with the TPM where one exists, without that operational cost.

### Encrypt the secrets files with a Hello-gated wrapping key

- **Pros**: no platform-store dependency.
- **Cons**: reimplements what DPAPI / Secret Service already do, and still
  needs the wrapping key somewhere. Kept as the follow-up for the interactive
  unlock gate, on top of this store.

### Keep files only, document BitLocker/LUKS

- **Pros**: no code, no dependency.
- **Cons**: leaves the current exposure for every user who did not encrypt
  their whole disk — the common desktop case.

## Reasoning

The threat is a disk or backup read, not a live host (out of scope per
`SECURITY.md`). The OS vaults exist precisely for that: user-scoped,
DPAPI/TPM-backed on Windows, daemon-unlocked on Linux. `auto` as the desktop
default migrates existing installs without a flag day; `require` gives the
paranoid a fail-closed promise.

## Trade-offs accepted

- The store is per-user, per-machine: moving `picsou.db` to another machine
  without the vault (or its export) makes its encrypted columns unreadable.
  The backup story must carry the vault entry, not just the database file.
- Linux without a Secret Service daemon or keyutils falls back to files in
  `auto` with a warning — the protection is opportunistic there, guaranteed
  only in `require` (which then refuses to start).
- The `keyring` dependency pulls the D-Bus stack on Linux (build time, not a
  runtime daemon requirement beyond what the fallback already tolerates).

## Consequences

- `backend-rs/src/crypto/keystore.rs` (store mapping) and `secrets.rs`
  (mode, migration, fallback) implement it; the desktop shell sets `auto`.
- The follow-up unlock gate (Windows Hello / Linux prompt before sessions are
  issued) builds on this store instead of on files.
- Docs: [`docs/features/os-keystore.md`](../features/os-keystore.md).

## Supersedes

None. Complements [2026-04-08-mandatory-encryption-key](2026-04-08-mandatory-encryption-key.md).
