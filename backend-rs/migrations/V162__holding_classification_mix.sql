-- Allow a holding classification to declare a multi-asset split (equity / bonds / monetary cash)
-- for mixed funds (e.g. FCPE or balanced funds) instead of a single wealth tier.
-- Either all three percentages are NULL (single-tier holding) or all three are set and sum to 100.
ALTER TABLE holding_classification
    ADD COLUMN equity_pct NUMERIC(5, 2),
    ADD COLUMN bonds_pct NUMERIC(5, 2),
    ADD COLUMN cash_pct NUMERIC(5, 2),
    ADD CONSTRAINT ck_holding_classification_mix CHECK (
        (equity_pct IS NULL AND bonds_pct IS NULL AND cash_pct IS NULL)
        OR (
            equity_pct IS NOT NULL AND bonds_pct IS NOT NULL AND cash_pct IS NOT NULL
            AND equity_pct >= 0 AND bonds_pct >= 0 AND cash_pct >= 0
            AND equity_pct + bonds_pct + cash_pct = 100
        )
    );
