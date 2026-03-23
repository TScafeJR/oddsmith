-- Date windows and capital limits for backtest runs.
-- date_from / date_to come from 001 and hold the run's window.
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS window_by       TEXT;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS enforce_capital BOOLEAN DEFAULT FALSE;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS skipped_trades  INTEGER;
ALTER TABLE backtest_runs ADD COLUMN IF NOT EXISTS final_equity    NUMERIC(20,6);

CREATE INDEX IF NOT EXISTS idx_backtest_runs_window ON backtest_runs(date_from, date_to);
