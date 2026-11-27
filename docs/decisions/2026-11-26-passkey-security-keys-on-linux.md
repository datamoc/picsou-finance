# ADR: Passkey sign-in with a USB security key on Linux

> Date: 2026-11-26
> Status: 🟡 Proposed

## Context

[Natixis passkey sign-in](./2026-11-25-natixis-passkey-on-desktop.md) reaches Windows only:
the shell asks `webauthn.dll`, which shows the system prompt. Linux has no such interface,
and WebKitGTK runs no WebAuthn ceremony. A saver whose passkey lives on a FIDO2 security key
can still plug it into a Linux machine.

## Decision

1. **The shell talks to the key itself**, over `hidraw`, with Mozilla's
   [`authenticator`](https://github.com/mozilla/authenticator-rs) crate, the one Firefox
   uses. It handles the PIN protocols, several keys and several passkeys on one key.
2. **Its pure-Rust crypto backend** (`crypto_rust`), so `libudev` is the one new system
   library.
3. **The shell draws the prompt** in its own window (`desktop/ui/passkey.html`). The PIN
   goes from that window to the shell over Tauri IPC, which only that window may call, and
   from the shell to the key. It reaches neither the backend nor the page the backend serves,
   and is never stored or logged.
4. **Everything else is unchanged:** the loopback route, the relying-party allow-list, the
   `clientDataJSON` built by the shell, one prompt at a time.

## Alternatives considered

### `libfido2`

- **Pros**: Yubico's reference library.
- **Cons**: a C library to install, bundle in the AppImage and bind to, and the PIN and
  device-selection steps to write ourselves.

### The `ctap-hid-fido2` crate

- **Pros**: pure Rust, small.
- **Cons**: no way to cancel a request in progress, and no handling of several keys.

### The `webauthn-authenticator-rs` crate

- **Pros**: a WebAuthn-level interface.
- **Cons**: it builds the client data itself and brings OpenSSL.

### A credentials desktop portal

- **Pros**: the system would draw the prompt, as on Windows.
- **Cons**: the distributions Picsou runs on do not ship one.

## Trade-offs accepted

- **USB security keys only.** No phone, no fingerprint reader of the computer, no password
  manager, no NFC reader.
- **The user must be allowed to open the key.** systemd's udev rules grant it to the seated
  user; a system without them needs the distribution's FIDO rules. The window says so when a
  plugged-in key is closed to the user.
- **One more window and its translations** to maintain.
- **`authenticator` is MPL-2.0** (file-level copyleft), used unmodified.

## Consequences

- New: `desktop/src-tauri/src/passkey/linux/`, `desktop/ui/passkey.html`, the
  `passkey-prompt` capability; `libudev-dev` to build, `libudev1` in the `.deb`.
- See [natixis-interepargne.md](../features/natixis-interepargne.md) and
  [desktop-app.md](../features/desktop-app.md).
