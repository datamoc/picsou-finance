# ADR: Natixis passkey sign-in through the desktop shell's Windows prompt

> Date: 2026-11-25
> Status: 🟡 Proposed

## Context

Some Natixis Interépargne accounts offer only a passkey, and Natixis asks others to
register one (#688). The connector signs in with a password and a code, so a
passkey-only account answers `MFA_TYPE_UNSUPPORTED`.

[The 2026-11-06 ADR](2026-11-06-natixis-interepargne-httpx-sidecar.md) notes that a
server cannot use a passkey. That holds for the Docker and Helm installs. Picsou
Desktop is different: it runs on the saver's own machine, next to their authenticator.

A passkey login was run from a native Windows program against a real account: the
Windows WebAuthn interface signed the identity provider's challenge, and the rest of
the login was unchanged.

## Decision

1. **The desktop shell asks Windows for the assertion** (`desktop/src-tauri/src/passkey`),
   through `webauthn.dll`, the interface browsers use. It calls it directly with
   `windows-sys`, which the shell already depends on.
2. **The backend reaches it over the loopback bank bridge** (`POST /passkey/assertion`,
   shared sidecar key). The shell sets `NATIXIS_PASSKEY_URL`; a value from the user's
   environment is dropped, and the backend accepts only a loopback URL.
3. **The shell decides what is signed.** It builds `clientDataJSON` itself, for an
   allow-list of relying parties that holds `auth-nie.natixis.com` only. One prompt is
   open at a time.
4. **Only the native connector has the route** (`POST /passkey` of the in-process
   Natixis sidecar). The session it returns is the same as after a password login, so
   syncs, expiry and reconnection do not change. No key or credential is stored.
5. **Picsou never registers a passkey.** It uses one the saver already has; the prompt
   appears at every sign-in.

## Alternatives considered

### Paste a session token from the browser (#716)

- **Pros**: works on every install, including Docker.
- **Cons**: the saver types a command in the browser console.

### Open the Natixis site in a WebView, as for BNP Paribas

- **Pros**: the bank's own page runs the prompt.
- **Cons**: Picsou would have to read the tokens out of a third-party page, and a window
  per bank is more code to keep in step with the site.

### The `webauthn-authenticator-rs` crate

- **Pros**: used by the proof of concept.
- **Cons**: it adds a dependency and an older copy of the `windows` crate for one call.

### The backend calls Windows itself

- **Pros**: no bridge route.
- **Cons**: platform code and `unsafe` in the crate the server also runs, and no window to
  attach the prompt to.

## Reasoning

The prompt belongs to the application the saver is looking at, and the shell already
serves the backend on loopback for the banks that need the saver at the screen.

## Trade-offs accepted

- **Desktop only.** This decision covers Windows; Linux followed with
  [USB security keys](./2026-11-26-passkey-security-keys-on-linux.md). macOS, Docker and Helm
  keep the password login.
- **A passkey held by a password manager's browser extension cannot be used**: Windows
  offers Windows Hello, security keys and the providers registered with it.
- **`webauthn.dll` is linked at load time**, so the app needs Windows 10 1809 or later.
- **Natixis can change this flow** like the rest of its site; a change fails with
  `UPSTREAM_FORMAT_CHANGED` or a refused passkey, never with a partial session.

## Consequences

- New: `desktop/src-tauri/src/passkey/`, `backend-rs/src/native_sidecars/natixis/passkey.rs`,
  `GET` and `POST /api/natixis/auth/passkey`, and a passkey button in the Natixis panel.
- See [natixis-interepargne.md](../features/natixis-interepargne.md) and
  [desktop-app.md](../features/desktop-app.md).
