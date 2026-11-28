-- A sixth wealth tier, BONDS: bonds, bond funds and life-insurance euro funds, which insurers
-- invest mostly in bonds. It gets its own allocation target.
--
-- bonds_pct defaults to 0 so every saved profile still sums to 100 and keeps meaning what the
-- member chose. A member who never saved targets has no row and reads the shipped defaults,
-- which give BONDS 20 % (taken from EQUITY): the DEFAULT below mirrors only the column's
-- value for a row written without it, not that default.
--
-- V160: follows V159 (linxea sidecar session).
ALTER TABLE member_allocation_profile
    ADD COLUMN bonds_pct NUMERIC(5, 2) NOT NULL DEFAULT 0,
    DROP CONSTRAINT ck_member_allocation_profile_sum,
    DROP CONSTRAINT ck_member_allocation_profile_pct,
    ADD CONSTRAINT ck_member_allocation_profile_sum
        CHECK (bonds_pct + real_estate_pct + equity_pct + crypto_pct + alternative_pct = 100),
    ADD CONSTRAINT ck_member_allocation_profile_pct
        CHECK (bonds_pct >= 0 AND real_estate_pct >= 0 AND equity_pct >= 0
           AND crypto_pct >= 0 AND alternative_pct >= 0);

ALTER TABLE holding_classification
    DROP CONSTRAINT ck_holding_classification_tier,
    ADD CONSTRAINT ck_holding_classification_tier CHECK (wealth_tier IS NULL OR wealth_tier IN
        ('SAFETY_NET', 'BONDS', 'REAL_ESTATE', 'EQUITY', 'CRYPTO', 'ALTERNATIVE'));

ALTER TABLE account_holding
    DROP CONSTRAINT ck_account_holding_provider_wealth_tier,
    ADD CONSTRAINT ck_account_holding_provider_wealth_tier CHECK (provider_wealth_tier IS NULL
        OR provider_wealth_tier IN ('SAFETY_NET', 'BONDS', 'REAL_ESTATE', 'EQUITY', 'CRYPTO', 'ALTERNATIVE'));
