-- Migration 0006: Add graph_distance column to account_scores
-- Stores the social graph relationship label (Mutual follow, Follows you,
-- You follow, Stranger) used as a scoring weight multiplier.

ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS graph_distance TEXT;
