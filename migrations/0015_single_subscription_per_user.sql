WITH ranked_subscriptions AS (
    SELECT id,
           telegram_user_id,
           ROW_NUMBER() OVER (
               PARTITION BY telegram_user_id
               ORDER BY current_period_end_at DESC NULLS LAST, updated_at DESC, id DESC
           ) AS row_num
    FROM telegram_subscriptions
),
subscription_rewrites AS (
    SELECT loser.id AS old_subscription_id,
           winner.id AS new_subscription_id
    FROM ranked_subscriptions loser
    JOIN ranked_subscriptions winner
      ON winner.telegram_user_id = loser.telegram_user_id
     AND winner.row_num = 1
    WHERE loser.row_num > 1
)
UPDATE telegram_payment_events tpe
SET subscription_id = sr.new_subscription_id
FROM subscription_rewrites sr
WHERE tpe.subscription_id = sr.old_subscription_id;

DELETE FROM telegram_subscriptions ts
USING (
    SELECT id
    FROM (
        SELECT id,
               ROW_NUMBER() OVER (
                   PARTITION BY telegram_user_id
                   ORDER BY current_period_end_at DESC NULLS LAST, updated_at DESC, id DESC
               ) AS row_num
        FROM telegram_subscriptions
    ) ranked
    WHERE row_num > 1
) duplicates
WHERE ts.id = duplicates.id;

ALTER TABLE telegram_subscriptions
    DROP CONSTRAINT IF EXISTS telegram_subscriptions_telegram_user_plan_unique;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM information_schema.table_constraints
        WHERE constraint_name = 'telegram_subscriptions_telegram_user_unique'
          AND table_name = 'telegram_subscriptions'
    ) THEN
        ALTER TABLE telegram_subscriptions
            ADD CONSTRAINT telegram_subscriptions_telegram_user_unique
            UNIQUE (telegram_user_id);
    END IF;
END $$;
