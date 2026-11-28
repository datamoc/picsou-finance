-- Mirrors PostgreSQL V160: the BONDS wealth tier and its allocation target. SQLite cannot alter
-- a CHECK, so the three tables are rebuilt with their columns in the same order, bonds_pct
-- last as PostgreSQL's ADD COLUMN puts it. bonds_pct is 0 on every saved profile, which keeps
-- its sum at 100. V7, not V6: another branch of this fork holds V6.
CREATE TABLE member_allocation_profile_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id INTEGER NOT NULL,
    monthly_essential_expenses TEXT,
    safety_net_months INTEGER NOT NULL DEFAULT 6,
    real_estate_pct TEXT NOT NULL DEFAULT '30.00',
    equity_pct TEXT NOT NULL DEFAULT '50.00',
    crypto_pct TEXT NOT NULL DEFAULT '10.00',
    alternative_pct TEXT NOT NULL DEFAULT '10.00',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    bonds_pct TEXT NOT NULL DEFAULT '0',
    CONSTRAINT uk_member_allocation_profile_member UNIQUE (member_id),
    CONSTRAINT ck_member_allocation_profile_expenses CHECK ((monthly_essential_expenses IS NULL) OR (dec_cmp(monthly_essential_expenses, '0') >= 0)),
    CONSTRAINT ck_member_allocation_profile_months CHECK ((safety_net_months >= 1) AND (safety_net_months <= 24)),
    CONSTRAINT ck_member_allocation_profile_pct CHECK ((dec_cmp(bonds_pct, '0') >= 0) AND (dec_cmp(real_estate_pct, '0') >= 0) AND (dec_cmp(equity_pct, '0') >= 0) AND (dec_cmp(crypto_pct, '0') >= 0) AND (dec_cmp(alternative_pct, '0') >= 0)),
    CONSTRAINT ck_member_allocation_profile_sum CHECK (dec_cmp(dec_add(dec_add(dec_add(dec_add(bonds_pct, real_estate_pct), equity_pct), crypto_pct), alternative_pct), '100') = 0),
    CONSTRAINT member_allocation_profile_member_id_fkey FOREIGN KEY (member_id) REFERENCES family_member(id) ON DELETE CASCADE
);

INSERT INTO member_allocation_profile_new (
    id, member_id, monthly_essential_expenses, safety_net_months, real_estate_pct, equity_pct,
    crypto_pct, alternative_pct, created_at, updated_at
)
SELECT
    id, member_id, monthly_essential_expenses, safety_net_months, real_estate_pct, equity_pct,
    crypto_pct, alternative_pct, created_at, updated_at
FROM member_allocation_profile;

DROP TABLE member_allocation_profile;
ALTER TABLE member_allocation_profile_new RENAME TO member_allocation_profile;

CREATE TABLE holding_classification_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id INTEGER NOT NULL,
    ticker TEXT NOT NULL,
    wealth_tier TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    sector_key TEXT,
    country_key TEXT,
    CONSTRAINT uk_holding_classification_member_ticker UNIQUE (member_id, ticker),
    CONSTRAINT ck_holding_classification_country CHECK ((country_key IS NULL) OR (length(country_key) = 2 AND country_key GLOB '[A-Z][A-Z]')),
    CONSTRAINT ck_holding_classification_tier CHECK ((wealth_tier IS NULL) OR (wealth_tier IN ('SAFETY_NET', 'BONDS', 'REAL_ESTATE', 'EQUITY', 'CRYPTO', 'ALTERNATIVE'))),
    CONSTRAINT holding_classification_member_id_fkey FOREIGN KEY (member_id) REFERENCES family_member(id) ON DELETE CASCADE
);

INSERT INTO holding_classification_new (
    id, member_id, ticker, wealth_tier, created_at, updated_at, sector_key, country_key
)
SELECT id, member_id, ticker, wealth_tier, created_at, updated_at, sector_key, country_key
FROM holding_classification;

DROP TABLE holding_classification;
ALTER TABLE holding_classification_new RENAME TO holding_classification;

CREATE TABLE account_holding_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL,
    ticker TEXT NOT NULL,
    name TEXT,
    quantity TEXT NOT NULL,
    average_buy_in TEXT,
    current_price TEXT,
    last_synced_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    quote_currency TEXT,
    provider_value_eur TEXT,
    provider_pnl_eur TEXT,
    provider_price_date TEXT,
    provider_wealth_tier TEXT,
    isin TEXT
        CONSTRAINT ck_account_holding_isin CHECK (
            isin IS NULL OR (
                length(isin) = 12
                AND isin GLOB '[A-Z][A-Z][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][0-9]'
            )
        ),
    CONSTRAINT account_holding_account_id_ticker_key UNIQUE (account_id, ticker),
    CONSTRAINT ck_account_holding_provider_wealth_tier CHECK ((provider_wealth_tier IS NULL) OR (provider_wealth_tier IN ('SAFETY_NET', 'BONDS', 'REAL_ESTATE', 'EQUITY', 'CRYPTO', 'ALTERNATIVE'))),
    CONSTRAINT ck_account_holding_quote_currency CHECK ((quote_currency IS NULL) OR (length(quote_currency) = 3 AND quote_currency GLOB '[A-Z][A-Z][A-Z]')),
    CONSTRAINT account_holding_account_id_fkey FOREIGN KEY (account_id) REFERENCES account(id) ON DELETE CASCADE
);

INSERT INTO account_holding_new (
    id, account_id, ticker, name, quantity, average_buy_in, current_price, last_synced_at,
    created_at, updated_at, quote_currency, provider_value_eur, provider_pnl_eur,
    provider_price_date, provider_wealth_tier, isin
)
SELECT
    id, account_id, ticker, name, quantity, average_buy_in, current_price, last_synced_at,
    created_at, updated_at, quote_currency, provider_value_eur, provider_pnl_eur,
    provider_price_date, provider_wealth_tier, isin
FROM account_holding;

DROP TABLE account_holding;
ALTER TABLE account_holding_new RENAME TO account_holding;

CREATE INDEX idx_account_holding_isin ON account_holding (isin) WHERE isin IS NOT NULL;
