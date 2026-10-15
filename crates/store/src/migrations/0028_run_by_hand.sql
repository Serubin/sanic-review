-- Whether you started a review by hand, which keeps it from being dropped
-- as stale while it waits for a slot.
ALTER TABLE runs ADD COLUMN by_hand INTEGER NOT NULL DEFAULT 0;
