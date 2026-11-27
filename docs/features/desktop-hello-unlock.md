# Feature: Windows Hello unlock for the desktop lock

> Last updated: 2026-11-27

## Context

With the desktop password lock on, opening Picsou means typing the password every time.
On Windows the machine already knows how to ask for consent — Windows Hello — so the
lock offers it as a second way in. The password stays: Hello never replaces it, and Linux
keeps the password only (no equivalent consent prompt there).

## How it works

The login page shows **Continuer avec Windows Hello** when all four hold: desktop mode,
lock enabled, inside the Tauri shell, and `hello_available()` true. The click verifies
Hello in native code, then the shell trades its per-launch token for a single-use unlock
code and walks the main window through `GET /api/auth/desktop-hello-bootstrap?token=…`,
which signs the primary account in exactly like a password login (same session, same
cookies, same audit shape).

### Key files

- `backend-rs/src/setup_auth/desktop.rs` — `hello_unlock` (shell-only issuance),
  `hello_bootstrap` (single-use redemption), `DesktopToken`, `hello_code_fresh`
- `backend-rs/src/lib.rs` — `desktop_hello_pending` single-use slot
- `backend-rs/src/setup_auth/mod.rs` — routes and `permit_all` entries
- `desktop/src-tauri/src/hello.rs` — `UserConsentVerifier` prompt, token trade, navigation
- `desktop/src-tauri/capabilities/desktop-hello.json`, `permissions/desktop-app.toml`
- `frontend/src/pages/login/LoginPage.tsx`, `frontend/src/features/desktop/hello.ts`

### Flow

```
login page → invoke hello_unlock (shell)
  → UserConsentVerifier prompt (system UI, cancellable)
  → POST /api/auth/desktop-hello-unlock (launch-token header only)
  → window.navigate(/api/auth/desktop-hello-bootstrap?token=<single-use, 2 min>)
  → 303 to / with session + gate cookies
```

## Technical choices

| Choice | Why | Rejected alternative |
|--------|-----|----------------------|
| `windows` crate `UserConsentVerifier`, via `windows-sys` COM init | The same system prompt as the OS sign-in; no credential ever created or stored | A passkey for `local.picsou` (registration ceremony + server-side verification for one prompt) |
| Single-use code, 2-minute TTL, consumed on read | Replay-proof even if the code leaks through history or logs | A direct session cookie hand-off to the WebView (no such Tauri API for HttpOnly cookies) |
| `DesktopToken` extractor (header only) | A page script holds the gate cookie too, so the cookie must never authorize issuance | Reusing `DesktopSessionGate` (would let page JS mint codes, bypassing Hello) |
| Re-check lock, account and MFA at redemption | Any of them may change in the two minutes between issuance and use | Trusting the issuance check alone (TOCTOU: disabling the lock must kill pending codes) |
| Audit `desktop.hello.unlocked` at redemption | The sign-in is the auditable event, not the prompt | No audit (password logins are audited; Hello must be too) |

## Gotchas / Pitfalls

- The code is taken from `desktop_hello_pending` on every read, valid or not: a wrong or
  stale code burns the slot, and redemption after disabling the lock is refused with
  `DESKTOP_LOCK_DISABLED`.
- `hello_unlock` mirrors `autologin` admission: primary account, no MFA. With MFA enabled
  Hello stays silent and the password (+TOTP) path applies.
- `UserConsentVerifier` cancellations come back as the string `'cancelled'`: the page
  shows nothing, it does not banner a user choice.
- Frontend `hello.ts` imports `@tauri-apps/api/core` dynamically: the web and Docker
  builds never bundle it, and outside the shell `helloAvailable()` is false without calling.
- The shell's `BackendTarget` is set after a successful backend start; a click before that
  reports "the backend is not ready".

## Tests

- `setup_auth::desktop::tests::hello_codes_expire_two_minutes_after_issue` — pure TTL edges.
- `foundation/desktop_mode.rs::windows_hello_unlock_issues_a_single_use_session` — cookie-only
  refused, issuance, lock-off refusal on both endpoints, redirect + session cookies, replay dead.
- `hello::imp::tests` — only `Verified` unlocks, `Canceled` stays silent, prompt i18n ×4.
- `LoginPage.test.tsx` Hello block — offered only in the shell with lock + Hello; silent
  cancel; error banner on failure.

## Links

- Related ADR: [2026-11-27-os-keystore](../decisions/2026-11-27-os-keystore.md) (the store this unlocks on top of)
- Next step: Linux consent prompt (fprintd), currently deferred by user choice
