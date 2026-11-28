-- Roadmap features schema (PostgreSQL V160, mirrors SQLite V7).
-- Adds: health_dismissal, contribution_limit (+ accounts), virtual_portfolio (+ accounts),
-- custom_taxonomy (+ categories + assignments), transaction_split, spending_event (+ links),
-- account.cost_basis_method, stock_split, and ai_chat_session / ai_chat_message.

CREATE TABLE health_dismissal (
    id           BIGSERIAL PRIMARY KEY,
    member_id    BIGINT       NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    issue_key    VARCHAR(160) NOT NULL,
    dismissed_at TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_health_dismissal_member_key UNIQUE (member_id, issue_key)
);
CREATE INDEX idx_health_dismissal_member ON health_dismissal(member_id);

CREATE TABLE contribution_limit (
    id            BIGSERIAL PRIMARY KEY,
    member_id     BIGINT         NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    name          VARCHAR(120)   NOT NULL,
    envelope_kind VARCHAR(40)    NOT NULL DEFAULT 'CUSTOM',
    limit_amount  NUMERIC(19, 4) NOT NULL CHECK (limit_amount > 0),
    period_kind   VARCHAR(20)    NOT NULL DEFAULT 'LIFETIME' CHECK (period_kind IN ('ANNUAL', 'LIFETIME')),
    period_year   INTEGER,
    currency      VARCHAR(10)    NOT NULL DEFAULT 'EUR',
    created_at    TIMESTAMPTZ    NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ    NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_contribution_limit_member ON contribution_limit(member_id);

CREATE TABLE contribution_limit_account (
    limit_id   BIGINT NOT NULL REFERENCES contribution_limit(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    PRIMARY KEY (limit_id, account_id)
);

CREATE TABLE virtual_portfolio (
    id          BIGSERIAL PRIMARY KEY,
    member_id   BIGINT       NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    name        VARCHAR(120) NOT NULL,
    description TEXT,
    color       VARCHAR(24)  NOT NULL DEFAULT '#3b82f6',
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_virtual_portfolio_member ON virtual_portfolio(member_id);

CREATE TABLE virtual_portfolio_account (
    portfolio_id BIGINT NOT NULL REFERENCES virtual_portfolio(id) ON DELETE CASCADE,
    account_id   BIGINT NOT NULL REFERENCES account(id) ON DELETE CASCADE,
    PRIMARY KEY (portfolio_id, account_id)
);

CREATE TABLE custom_taxonomy (
    id          BIGSERIAL PRIMARY KEY,
    member_id   BIGINT       NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    name        VARCHAR(120) NOT NULL,
    description TEXT,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_custom_taxonomy_member ON custom_taxonomy(member_id);

CREATE TABLE custom_taxonomy_category (
    id             BIGSERIAL PRIMARY KEY,
    taxonomy_id    BIGINT        NOT NULL REFERENCES custom_taxonomy(id) ON DELETE CASCADE,
    name           VARCHAR(120)  NOT NULL,
    color          VARCHAR(24)   NOT NULL DEFAULT '#6366f1',
    target_percent NUMERIC(7, 4),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_custom_taxonomy_category_tax ON custom_taxonomy_category(taxonomy_id);

CREATE TABLE holding_taxonomy_assignment (
    id             BIGSERIAL PRIMARY KEY,
    member_id      BIGINT        NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    taxonomy_id    BIGINT        NOT NULL REFERENCES custom_taxonomy(id) ON DELETE CASCADE,
    category_id    BIGINT        NOT NULL REFERENCES custom_taxonomy_category(id) ON DELETE CASCADE,
    ticker         VARCHAR(60)   NOT NULL,
    weight_percent NUMERIC(7, 4) NOT NULL DEFAULT 100 CHECK (weight_percent > 0 AND weight_percent <= 100),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_holding_taxonomy_assignment UNIQUE (member_id, taxonomy_id, category_id, ticker)
);
CREATE INDEX idx_holding_taxonomy_member ON holding_taxonomy_assignment(member_id, taxonomy_id);

CREATE TABLE transaction_split (
    id             BIGSERIAL PRIMARY KEY,
    transaction_id BIGINT         NOT NULL REFERENCES transaction(id) ON DELETE CASCADE,
    category_id    BIGINT         NOT NULL REFERENCES category(id) ON DELETE CASCADE,
    amount         NUMERIC(19, 4) NOT NULL,
    note           VARCHAR(255),
    created_at     TIMESTAMPTZ    NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_transaction_split_tx ON transaction_split(transaction_id);

CREATE TABLE spending_event (
    id            BIGSERIAL PRIMARY KEY,
    member_id     BIGINT         NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    name          VARCHAR(120)   NOT NULL,
    description   TEXT,
    start_date    DATE,
    end_date      DATE,
    budget_amount NUMERIC(19, 4),
    color         VARCHAR(24)    NOT NULL DEFAULT '#3b82f6',
    created_at    TIMESTAMPTZ    NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ    NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_spending_event_member ON spending_event(member_id);

CREATE TABLE spending_event_transaction (
    event_id       BIGINT NOT NULL REFERENCES spending_event(id) ON DELETE CASCADE,
    transaction_id BIGINT NOT NULL REFERENCES transaction(id) ON DELETE CASCADE,
    PRIMARY KEY (event_id, transaction_id)
);

ALTER TABLE account
    ADD COLUMN cost_basis_method VARCHAR(16) NOT NULL DEFAULT 'AVERAGE'
    CONSTRAINT ck_account_cost_basis_method CHECK (cost_basis_method IN ('AVERAGE', 'FIFO', 'LIFO', 'HIFO'));

CREATE TABLE stock_split (
    id                BIGSERIAL PRIMARY KEY,
    member_id         BIGINT         NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    ticker            VARCHAR(60)    NOT NULL,
    split_date        DATE           NOT NULL,
    ratio_numerator   NUMERIC(19, 6) NOT NULL CHECK (ratio_numerator > 0),
    ratio_denominator NUMERIC(19, 6) NOT NULL DEFAULT 1 CHECK (ratio_denominator > 0),
    note              VARCHAR(255),
    created_at        TIMESTAMPTZ    NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_stock_split_member_ticker ON stock_split(member_id, ticker);

CREATE TABLE ai_chat_session (
    id         BIGSERIAL PRIMARY KEY,
    member_id  BIGINT       NOT NULL REFERENCES family_member(id) ON DELETE CASCADE,
    title      VARCHAR(160) NOT NULL,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_ai_chat_session_member ON ai_chat_session(member_id);

CREATE TABLE ai_chat_message (
    id         BIGSERIAL PRIMARY KEY,
    session_id BIGINT       NOT NULL REFERENCES ai_chat_session(id) ON DELETE CASCADE,
    role       VARCHAR(16)  NOT NULL CHECK (role IN ('USER', 'ASSISTANT')),
    content    TEXT         NOT NULL,
    provider   VARCHAR(40),
    model      VARCHAR(120),
    created_at TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_ai_chat_message_session ON ai_chat_message(session_id);
