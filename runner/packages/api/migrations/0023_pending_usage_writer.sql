REVOKE UPDATE (input_tokens, output_tokens, cached_tokens, cache_write_tokens, cost_usd)
  ON usage_events FROM rb_scheduler;

CREATE FUNCTION reconcile_pending_usage(
  reservation text, cost numeric, input_count bigint, output_count bigint, cached_count bigint, cache_write_count bigint,
  override_hold boolean
) RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
DECLARE
  held public.ledger_reservations%ROWTYPE;
BEGIN
  SELECT * INTO held FROM public.ledger_reservations WHERE id = reservation
    AND kind = 'model' AND status = 'settled' AND usage_pending FOR UPDATE;
  IF NOT FOUND OR cost IS NULL OR cost NOT BETWEEN 0 AND 99999999.999999
    OR (cost > held.amount_usd AND override_hold IS DISTINCT FROM true) THEN
    RETURN false;
  END IF;
  UPDATE public.usage_events
    SET cost_usd = cost, input_tokens = input_count, output_tokens = output_count,
        cached_tokens = cached_count, cache_write_tokens = cache_write_count
    WHERE reservation_id = reservation;
  IF NOT FOUND THEN
    RETURN false;
  END IF;
  INSERT INTO public.audit_log (id, actor, action, resource_type, resource_id, group_id, runner_id, detail)
  VALUES (gen_random_uuid()::text, '{"kind":"system","id":"ledger-operator"}', 'model.usage_reconciled',
    'reservation', reservation, held.group_id, held.runner_id,
    jsonb_build_object('previous_usd', held.actual_usd::text, 'actual_usd', cost::numeric(14,6)::text,
      'override_hold', cost > held.amount_usd, 'usage', jsonb_build_object(
        'input_tokens', input_count, 'output_tokens', output_count,
        'cached_tokens', cached_count, 'cache_write_tokens', cache_write_count)));
  RETURN true;
END;
$$;
REVOKE ALL ON FUNCTION reconcile_pending_usage(text, numeric, bigint, bigint, bigint, bigint, boolean) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION reconcile_pending_usage(text, numeric, bigint, bigint, bigint, bigint, boolean) TO rb_scheduler;
