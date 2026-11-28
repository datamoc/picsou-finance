# Standalone Roadmap Features

Overview of the pure-Rust standalone wealth, analytics, security, and budgeting features added in
PostgreSQL `V160__roadmap_features.sql` / SQLite `V7__roadmap_features.sql`.

## Security & Standalone Storage

- **Password-protected encrypted backups** (`backend-rs/src/transfer/encrypted_backup.rs`):
  `POST /api/backup/encrypted/export`, `/verify`, `/restore`, and `GET /api/backup/storage-security`.
  Uses `VACUUM INTO` for consistent SQLite snapshots, gzip compression, PBKDF2-HMAC-SHA256
  (210,000 iterations), and AES-256-GCM authenticated encryption.
- **SQLite at-rest vault envelope** (`backend-rs/src/db/sqlite/vault.rs`):
  domain-separated AES-256-GCM vault sealing/unsealing (`PICSOU_DB_ENCRYPTION=off|auto|require`).
- **Isolated standalone profiles** (`backend-rs/src/desktop_profiles.rs`, `desktop/src-tauri/src/main.rs`):
  `profiles.json` registry adopting existing root `picsou.db` as the `default` profile and
  isolating additional profiles under `profiles/<uuid>/`.

## Portfolio & Wealth Analytics

- **Data Health Center** (`backend-rs/src/wealth/health.rs`, `/health`):
  evaluates price staleness, unclassified holdings, missing cost basis, foreign-currency exposure,
  stale syncs, and snapshot gaps with severity scaling (`CRITICAL` above 30% net worth) and
  persistent dismissals (`health_dismissal`).
- **Contribution Limits** (`backend-rs/src/wealth/limits.rs`, `/api/contribution-limits`):
  tracks lifetime and annual deposit caps with French statutory presets (PEA, PEA-PME, Livret A,
  LDDS, LEP, Livret Jeune, PEL, PER).
- **Rebalancing Worksheet** (`backend-rs/src/wealth/rebalance.rs`, `/api/analysis/rebalance-plan`):
  configurable drift band, drift drivers, buy/sell orders, cash-only no-sell optimization, and CSV export.
- **Performance Engine (TWR / MWR-IRR + Benchmarks)** (`backend-rs/src/wealth/performance.rs`):
  daily-linked time-weighted return, XIRR / Modified Dietz money-weighted return, max drawdown,
  annualized volatility, snapshot-vs-transaction tracking mode counts, Base-100 benchmark overlay,
  and SVG export.
- **Retirement / FIRE & Save-Up Simulators** (`backend-rs/src/wealth/planning.rs`):
  FI number, Coast FIRE, real/nominal accumulation and retirement decumulation curve, longevity
  depletion age, and compound save-up calculator.
- **Virtual Portfolios** (`backend-rs/src/wealth/portfolios.rs`, `/api/portfolios`):
  custom named account groupings linked directly to Performance analysis.
- **Custom Taxonomies** (`backend-rs/src/wealth/taxonomies.rs`, `/api/taxonomies`):
  user-defined classification axes with categories, target shares, and holding assignments.
- **Consolidated Capital Income** (`backend-rs/src/wealth/income.rs`, `/api/wealth/income`):
  monthly dividend, interest, and SCPI history with YoY delta and unconverted-currency alerts.

## Market Data, AI, Budgeting & Cost Basis

- **Resilient Market Data Chain** (`backend-rs/src/market/circuit_breaker.rs`,
  `finance_database.rs`, `providers_extra.rs`):
  per-provider circuit breaker, offline FinanceDatabase security catalog (`/api/market/search`),
  and secondary fallback providers (Börse Frankfurt, EODHD, Marketstack).
- **Integrated AI Assistant & AI CSV Mapper** (`backend-rs/src/ai/chat.rs`, `csv_mapper.rs`, `/assistant`):
  persistent chat sessions grounded in live portfolio snapshots and LLM-assisted CSV column mapping
  with deterministic multilingual fallback.
- **Budget Splits & Spending Events** (`backend-rs/src/budget/splits.rs`, `events.rs`, `/budget/events`):
  exact-sum transaction splitting across categories and cross-category project/trip spending events.
- **Configurable Cost Basis (`AVERAGE` / `FIFO` / `LIFO` / `HIFO`) & Stock Splits**
  (`backend-rs/src/accounts/realized_pnl.rs`):
  per-account lot accounting method and chronological stock-split adjustment.
- **Display Quick Wins**:
  compact figures toggle (`compactNumbers` via `useMoney()`) and offline-safe SVG chart export
  (`ExportSvgButton`).
