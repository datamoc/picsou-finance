-- Mirrors PostgreSQL V162: optional equity/bonds/cash percentage split on holding_classification
-- for mixed/complex funds (e.g. 75% equity + 25% monetary cash).
ALTER TABLE holding_classification
    ADD COLUMN equity_pct TEXT
        CHECK (equity_pct IS NULL OR (dec_cmp(equity_pct, '0') >= 0 AND dec_cmp(equity_pct, '100') <= 0));

ALTER TABLE holding_classification
    ADD COLUMN bonds_pct TEXT
        CHECK (bonds_pct IS NULL OR (dec_cmp(bonds_pct, '0') >= 0 AND dec_cmp(bonds_pct, '100') <= 0));

ALTER TABLE holding_classification
    ADD COLUMN cash_pct TEXT
        CHECK (cash_pct IS NULL OR (dec_cmp(cash_pct, '0') >= 0 AND dec_cmp(cash_pct, '100') <= 0));
