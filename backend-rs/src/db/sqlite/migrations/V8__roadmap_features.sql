-- Roadmap features schema (SQLite V7, mirrors PostgreSQL V160__roadmap_features.sql).
-- All numeric values are stored as exact TEXT decimals (zero REAL/FLOAT columns).

CREATE TABLE health_dismissal (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id    INTEGER NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    issue_key    TEXT    NOT NULL,
    dismissed_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    UNIQUE (member_id, issue_key)
);
CREATE INDEX idx_health_dismissal_member ON health_dismissal(member_id);

CREATE TABLE contribution_limit (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id     INTEGER NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    envelope_kind TEXT    NOT NULL DEFAULT 'CUSTOM',
    limit_amount  TEXT    NOT NULL,
    period_kind   TEXT    NOT NULL DEFAULT 'LIFETIME' CHECK (period_kind IN ('ANNUAL', 'LIFETIME')),
    period_year   INTEGER,
    currency      TEXT    NOT NULL DEFAULT 'EUR',
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now'))
);
CREATE INDEX idx_contribution_limit_member ON contribution_limit(member_id);

CREATE TABLE contribution_limit_account (
    limit_id   INTEGER NOT NULL REFERENCES contribution_limit(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    PRIMARY KEY (limit_id, account_id)
);

CREATE TABLE virtual_portfolio (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id   INTEGER NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    description TEXT,
    color       TEXT    NOT NULL DEFAULT '#3b82f6',
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now'))
);
CREATE INDEX idx_virtual_portfolio_member ON virtual_portfolio(member_id);

CREATE TABLE virtual_portfolio_account (
    portfolio_id INTEGER NOT NULL REFERENCES virtual_portfolio(id) ON DELETE CASCADE,
    account_id   INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    PRIMARY KEY (portfolio_id, account_id)
);

CREATE TABLE custom_taxonomy (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id   INTEGER NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    description TEXT,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now'))
);
CREATE INDEX idx_custom_taxonomy_member ON custom_taxonomy(member_id);

CREATE TABLE custom_taxonomy_category (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    taxonomy_id    INTEGER NOT NULL REFERENCES custom_taxonomy(id) ON DELETE CASCADE,
    name           TEXT    NOT NULL,
    color          TEXT    NOT NULL DEFAULT '#6366f1',
    target_percent TEXT,
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now'))
);
CREATE INDEX idx_custom_taxonomy_category_tax ON custom_taxonomy_category(taxonomy_id);

CREATE TABLE holding_taxonomy_assignment (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id      INTEGER NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    taxonomy_id    INTEGER NOT NULL REFERENCES custom_taxonomy(id) ON DELETE CASCADE,
    category_id    INTEGER NOT NULL REFERENCES custom_taxonomy_category(id) ON DELETE CASCADE,
    ticker         TEXT    NOT NULL,
    weight_percent TEXT    NOT NULL DEFAULT '100',
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    UNIQUE (member_id, taxonomy_id, category_id, ticker)
);
CREATE INDEX idx_holding_taxonomy_member ON holding_taxonomy_assignment(member_id, taxonomy_id);

CREATE TABLE transaction_split (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    transaction_id INTEGER NOT NULL REFERENCES "transaction"(id) ON DELETE CASCADE,
    category_id    INTEGER NOT NULL REFERENCES category(id) ON DELETE CASCADE,
    amount         TEXT    NOT NULL,
    note           TEXT,
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now'))
);
CREATE INDEX idx_transaction_split_tx ON transaction_split(transaction_id);

CREATE TABLE spending_event (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id     INTEGER NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    description   TEXT,
    start_date    TEXT,
    end_date      TEXT,
    budget_amount TEXT,
    color         TEXT    NOT NULL DEFAULT '#3b82f6',
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now'))
);
CREATE INDEX idx_spending_event_member ON spending_event(member_id);

CREATE TABLE spending_event_transaction (
    event_id       INTEGER NOT NULL REFERENCES spending_event(id) ON DELETE CASCADE,
    transaction_id INTEGER NOT NULL REFERENCES "transaction"(id) ON DELETE CASCADE,
    PRIMARY KEY (event_id, transaction_id)
);

ALTER TABLE account
    ADD COLUMN cost_basis_method TEXT NOT NULL DEFAULT 'AVERAGE'
    CHECK (cost_basis_method IN ('AVERAGE', 'FIFO', 'LIFO', 'HIFO'));

CREATE TABLE stock_split (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id         INTEGER NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    ticker            TEXT    NOT NULL,
    split_date        TEXT    NOT NULL,
    ratio_numerator   TEXT    NOT NULL,
    ratio_denominator TEXT    NOT NULL DEFAULT '1',
    note              TEXT,
    created_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now'))
);
CREATE INDEX idx_stock_split_member_ticker ON stock_split(member_id, ticker);

CREATE TABLE ai_chat_session (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id  INTEGER NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    title      TEXT    NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now'))
);
CREATE INDEX idx_ai_chat_session_member ON ai_chat_session(member_id);

CREATE TABLE ai_chat_message (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES ai_chat_session(id) ON DELETE CASCADE,
    role       TEXT    NOT NULL CHECK (role IN ('USER', 'ASSISTANT')),
    content    TEXT    NOT NULL,
    provider   TEXT,
    model      TEXT,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f000Z', 'now'))
);
CREATE INDEX idx_ai_chat_message_session ON ai_chat_message(session_id);
