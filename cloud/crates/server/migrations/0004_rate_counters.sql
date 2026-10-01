-- Rate limits that have to hold across every instance (serverless hosts run many short-lived
-- ones, each with its own memory): a counter per key per fixed window. Rows older than a day go
-- with the daily cleanup.
CREATE TABLE rate_counters (
    key           text NOT NULL,
    window_start  timestamptz NOT NULL,
    count         integer NOT NULL,
    PRIMARY KEY (key, window_start)
);
