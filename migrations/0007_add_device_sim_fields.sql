ALTER TABLE devices
ADD COLUMN IF NOT EXISTS sim_card TEXT;

ALTER TABLE devices
ADD COLUMN IF NOT EXISTS sim_card_expiration_date DATE;
