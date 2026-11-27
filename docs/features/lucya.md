# Feature: Lucya contracts (life insurance and PER)

> Last updated: 2026-11-27

## Context

[Lucya](https://app.lucya.com) distributes life insurance and retirement (PER) contracts and has
no API that Picsou can use. The member refreshes the value of their contracts by signing in from
the Sync page.

## How it works

`POST /api/lucya/refresh` (`{identifier, password}`) does everything in one call: it signs in on
the site, reads the contracts of its home page and writes one account per contract. Nothing about
the sign-in is stored, neither a session nor the password, so there is no scheduled sync, no
"Sync all" entry and no disconnection.

### Key files

- `backend-rs/src/native_sidecars/lucya.rs`: sign-in and home-page parser, no database access
- `backend-rs/src/connectors/lucya/mod.rs`: the route and the account import
- `frontend/src/components/sync/LucyaPanel.tsx`: the form on the Sync page

### Flow

```
GET  /asv/espace-client/connexion      form token and session cookie
POST /asv/espace-client/connexion      identifier and password, once
     -> home page                      contracts and announced total
     -> accounts + today's snapshot    one transaction
```

Each contract becomes an `ASSURANCE_VIE` or `PER` account of provider `Lucya`, with external id
`lucya_<id in the contract page's address>`, no holdings, and the value of the home page. A refresh
renames and revalues the account, restores a listed contract the member had deleted, and closes at
zero an imported contract the page no longer lists.

| Code | Meaning |
|------|---------|
| `INVALID_CREDENTIALS` | the form came back |
| `MFA_TYPE_UNSUPPORTED` | the site asks for a second factor |
| `CAPTCHA_BLOCKED` | the site asks for a captcha |
| `PORTFOLIO_INCOMPLETE` | the contracts do not add up to the announced total |
| `UPSTREAM_FORMAT_CHANGED` | a page, a field or a contract type is not the expected one |
| `RATE_LIMITED`, `UPSTREAM_UNAVAILABLE` | the site refused for now or could not be reached |

Every refusal writes nothing. The route allows 5 attempts per 15 minutes and per address.

## Technical choices

| Choice | Why | Rejected alternative |
|--------|-----|----------------------|
| Refresh by signing in, nothing stored | A stored session needs `sidecar_session` to accept a new bank, which means rebuilding its CHECK constraints in a migration. Each bank added upstream rebuilds them too, so a migration of this fork would conflict with the next ones | A scheduled sync with a stored session |
| One account value per contract, no holdings | Only the home page is read | Fund lines from the contract page |
| Refuse an unknown contract type | A contract filed under a guessed envelope would be wrong without anyone noticing | Fall back to life insurance |

## Gotchas / Pitfalls

- **No gain or loss.** The home page reports no amount paid in: the invested amount is the value
  itself.
- **The snapshot is dated today**, as for other connectors, not at the valuation date the page
  prints. A second refresh the same day overwrites it.
- **A second factor is not driven.** The site has a setting for it; an account that turns it on can
  no longer be refreshed.
- The password is posted once: a redirect that would repeat it, or leave the site, is refused. The
  sign-in is never retried and the site's logout is never called.
- `LUCYA_BASE_URL` only accepts a loopback address (tests), so no setting can send the password
  elsewhere.
- Logs carry the step, a status and a size, never the identifier, a token or page content.
- The parser relies on the home page's classes (`contract`, `contract__name`, `contract__price`,
  `contract__date`, `card--important`). Fixtures are invented: never add a real page.

## Tests

- `native_sidecars::lucya` unit tests: sign-in outcomes, parser, address guard
- `connectors::lucya` unit test: contract type wording
- `backend-rs/tests/integration/connectors/lucya.rs`: accounts written, updated, restored and
  closed; refusals write nothing; no session row
- `frontend/src/pages/sync/LucyaTab.test.tsx`: the form, success and error messages
