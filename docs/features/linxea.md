# Feature: Linxea sync

## Scope

The connector imports Linxea savings contracts as Picsou `PER`, `ASSURANCE_VIE`, or
`PEA` accounts directly inside Picsou Desktop (100% pure Rust, zero Docker, zero
Python). A successful snapshot contains:

- the authoritative contract valuation in EUR (sum of every support line);
- every unitised support (ETF, mutual fund, etc.) with its ISIN, unit count,
  EUR average buying price (`Prix de Revient Moyen`), latest quote, valuation,
  and unrealized P&L;
- every non-unitised support (*fonds euro*) represented as one unit at its
  authoritative EUR valuation, with its cost basis derived from `value - pnl`
  when Linxea exports `0.00` as average buy-in.

Real-estate (`RealEstate` / SCPI) contracts are skipped because their export is
not a savings-support workbook. Transactions and historical statements are out
of scope; Picsou records one daily balance snapshot per successful import.

This is an unofficial, read-only integration designed to fail closed: an
unreadable workbook, missing required column, or inconsistent contract total
never overwrites the last valid portfolio.

## Authentication (native OS WebView)

On Picsou Desktop (Windows `WebView2`, macOS `WKWebView`, Linux `WebKitGTK`),
`desktop/src-tauri/src/webview_bank/linxea.rs` runs directly inside the Rust
desktop binary with no Docker and no Python runtime:

1. `POST /api/linxea/webview/start` opens a native incognito OS WebView window
   on `https://espaceclient.linxea.com/authentification` (restricted to HTTPS
   `*.linxea.com` main-frame navigations).
2. The user signs in directly on Linxea's official page (e-mail, password when
   prompted, and 6-digit e-mail verification code). Picsou never sees or stores
   credentials.
3. Once the WebView lands on `/epargne` (or `/epargne/contrat/<uuid>`) and
   `oidc.user:*` holds a valid `access_token` and `profile.sub`, the Rust bridge
   calls `contracts-api.linxea.com` via `reqwest`, parses each XLSX workbook in
   pure Rust (`backend-rs/src/connectors/linxea/export_parser.rs`), caches the
   snapshot for the immediate backend sync, stores the encrypted session
   envelope in `sidecar_session` (`bank = 'LINXEA'`), and closes the window.
4. Subsequent syncs call `contracts-api.linxea.com` directly via `reqwest`
   (attempting an OIDC `refresh_token` exchange if expired) without reopening a
   window, falling back to `SESSION_EXPIRED` when re-login is needed.

## Contract collection and pure-Rust XLSX export parser

Rather than scraping rendered DOM tables, the Rust bridge calls Linxea's own
contracts API:

- `GET https://contracts-api.linxea.com/api/v2/users/{userId}/contracts`
- `GET https://contracts-api.linxea.com/api/v2/users/{userId}/contracts/{contractId}/export`

Each contract's official XLSX workbook ("données financières", single sheet
`Tous les comptes`) is parsed directly in Rust (`zip` + `quick-xml`,
size-bounded and header-keyed) in
`backend-rs/src/connectors/linxea/export_parser.rs`. Non-unitised supports
receive a `LINXEA_<slug>` ticker that Picsou classifies as an internal fund code
(`LINXEA_FUND_CODE_PREFIX`) so market-price providers are never queried for it.

## Asynchronous import and persistence

Authentication stores the encrypted session and queues an import on the shared
`SyncTemplate` (`IDLE -> QUEUED -> RUNNING -> SUCCESS | FAILED`). Manual
`POST /api/linxea/sync` returns `202 Accepted` immediately; the UI polls
`GET /api/linxea/status`.

After every contract passes reconciliation (`sum(positions) ~= balance`), a
short database transaction:

- upserts accounts under stable `linxea_<contractId>` external IDs and provider
  `Linxea`;
- preserves user-soft-deleted accounts instead of recreating them;
- replaces each account's holdings and records the daily balance snapshot;
- marks the session `SUCCESS`.

Only `SESSION_EXPIRED` deactivates the stored session (and maps to
`NEEDS_REAUTH` in the daily scheduler). Transient upstream or format failures
leave the existing session and holdings intact.

## Frontend

- `frontend/src/components/sync/LinxeaPanel.tsx` wraps `SidecarSessionPanel`
  (`provider="linxea"`, `translationPrefix="sync.linxea"`, `webview={...}`).
- Exposed in `SyncPage`, `AddAccountModal`, `SyncAllModal`, and
  `connector-report.ts` (`linxea`), with full `en`/`fr`/`de`/`es` translations.

## Verification boundaries

Unit tests in `backend-rs/src/connectors/linxea/export_parser.rs` cover
header-keyed XLSX parsing (including zero-part euro funds with `0.00` average
buy-in, `N° Contrat` header normalization, and OpenXML `t="str"` cells).
Desktop unit tests in `desktop/src-tauri/src/webview_bank/linxea.rs` cover host
allowlisting, authenticated route detection, OIDC key parsing, and session
encoding. Backend integration tests in
`backend-rs/tests/integration/connectors/linxea.rs` verify Desktop WebView
login, auth, rate limiting, reconciliation, atomic upsert, soft-delete
preservation, member isolation, and startup recovery using synthetic fixtures
only. Frontend tests cover `LinxeaTab`, hooks, and providers.
